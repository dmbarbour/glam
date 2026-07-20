use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use bytes::Bytes;
use glam::reflection::{
    CommitResult, ConflictAnalysisStrategy, EffectRequestSpec, EffectRun, ExactConflictAnalysis,
    HostSnapshot, ReflectionEffects, ReflectionHost, ReflectionJournal, ReflectionRequest,
    ReflectionServices, ReflectionStore, ReflectionTransaction, RequestContext, RequestResult,
    TaskCommit, TaskEnvironment, TaskHost, TaskOutcome, TaskSpecialization,
    handle_reflection_request, reflection_request_specs,
};
use glam::{
    Assembler, Builtin, Diagnostic, DiagnosticBus, DiagnosticEvent, DiagnosticSubscriber, Error,
    EvaluationRuntime, FileSourceSystem, ModuleInput, ReasoningReport, ReasoningStatus,
    ReasoningTaskState, Severity, Value, check_local_manifest,
};

// Parse inspection intentionally remains on the front-end API while ordinary
// assembly uses only the embedding facade.
use glam::g_syntax::{DeclarationKind, ParsedSource, parse_source};

#[derive(Default)]
struct AssemblyCommand {
    inputs: Vec<ModuleInput>,
    arguments: Vec<String>,
    reflection_arguments: Vec<String>,
    manifest: Option<PathBuf>,
    worker_threads: Option<usize>,
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
        "--check_manifest" => check_manifest_command(args, false),
        "--quiet" => match args.next().as_deref() {
            Some("--check_manifest") => check_manifest_command(args, true),
            Some(option) => {
                eprintln!(
                    "error: `--quiet` is currently supported only with `--check_manifest`, got `{option}`"
                );
                ExitCode::from(2)
            }
            None => {
                eprintln!("error: `--quiet` needs `--check_manifest <PATH>`");
                ExitCode::from(2)
            }
        },
        "-f" | "--file" => {
            let Some(path) = args.next() else {
                eprintln!("error: `{}` needs a source path", first);
                return ExitCode::from(2);
            };

            run_assembly(
                args,
                AssemblyCommand {
                    inputs: vec![ModuleInput::file(path)],
                    ..AssemblyCommand::default()
                },
            )
        }
        option if script_extension(option).is_some() => {
            let extension = script_extension(option).expect("checked above").to_owned();
            let Some(body) = args.next() else {
                eprintln!("error: `{option}` needs a script body");
                return ExitCode::from(2);
            };

            run_assembly(
                args,
                AssemblyCommand {
                    inputs: vec![ModuleInput::script(extension, body)],
                    ..AssemblyCommand::default()
                },
            )
        }
        "--manifest" => {
            let Some(path) = args.next() else {
                eprintln!("error: `--manifest` needs an output path");
                return ExitCode::from(2);
            };
            run_assembly(
                args,
                AssemblyCommand {
                    manifest: Some(PathBuf::from(path)),
                    ..AssemblyCommand::default()
                },
            )
        }
        "--refl" => {
            let Some(argument) = args.next() else {
                eprintln!("error: `--refl` needs an argument");
                return ExitCode::from(2);
            };
            run_assembly(
                args,
                AssemblyCommand {
                    reflection_arguments: vec![argument],
                    ..AssemblyCommand::default()
                },
            )
        }
        "--workers" => {
            let Some(count) = args.next() else {
                eprintln!("error: `--workers` needs a non-negative integer");
                return ExitCode::from(2);
            };
            let Ok(worker_threads) = parse_worker_count(&count, "--workers") else {
                return ExitCode::from(2);
            };
            run_assembly(
                args,
                AssemblyCommand {
                    worker_threads: Some(worker_threads),
                    ..AssemblyCommand::default()
                },
            )
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

fn check_manifest_command(mut args: impl Iterator<Item = String>, leading_quiet: bool) -> ExitCode {
    let Some(manifest) = args.next() else {
        eprintln!("error: `--check_manifest` needs a manifest path");
        return ExitCode::from(2);
    };
    if manifest.starts_with('-') {
        eprintln!("error: `--check_manifest` needs a manifest path before any options");
        return ExitCode::from(2);
    }
    let quiet = match args.next().as_deref() {
        None => leading_quiet,
        Some("--quiet") if !leading_quiet => true,
        Some("--quiet") => {
            eprintln!("error: `--quiet` may be specified only once");
            return ExitCode::from(2);
        }
        Some(option) if option.starts_with('-') => {
            eprintln!("error: unknown `--check_manifest` option `{option}`");
            return ExitCode::from(2);
        }
        Some(_) => {
            eprintln!(
                "error: `--check_manifest` accepts only a manifest path and optional `--quiet`"
            );
            return ExitCode::from(2);
        }
    };
    if args.next().is_some() {
        eprintln!("error: `--check_manifest` accepts only a manifest path and optional `--quiet`");
        return ExitCode::from(2);
    }
    let manifest = PathBuf::from(manifest);

    match check_local_manifest(&manifest) {
        Ok(mismatches) if mismatches.is_empty() => ExitCode::SUCCESS,
        Ok(mismatches) => {
            if !quiet {
                let mut stdout = io::stdout().lock();
                for mismatch in mismatches {
                    if writeln!(stdout, "{mismatch}").is_err() {
                        eprintln!("error: could not write manifest check results to stdout");
                        return ExitCode::from(1);
                    }
                }
            }
            ExitCode::from(1)
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn run_assembly(args: impl Iterator<Item = String>, mut command: AssemblyCommand) -> ExitCode {
    if let Err(exit_code) = collect_assembly_inputs(args, &mut command) {
        return exit_code;
    }
    if command.inputs.is_empty() {
        eprintln!("error: assembly needs at least one `--file` or `--script.<ext>` input");
        return ExitCode::from(2);
    }
    assemble_inputs(command)
}

fn collect_assembly_inputs(
    mut args: impl Iterator<Item = String>,
    command: &mut AssemblyCommand,
) -> Result<(), ExitCode> {
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                command.arguments.extend(args);
                return Ok(());
            }
            "-f" | "--file" => {
                let Some(path) = args.next() else {
                    eprintln!("error: `{arg}` needs a source path");
                    return Err(ExitCode::from(2));
                };
                command.inputs.push(ModuleInput::file(path));
            }
            "--manifest" => {
                let Some(path) = args.next() else {
                    eprintln!("error: `--manifest` needs an output path");
                    return Err(ExitCode::from(2));
                };
                if command.manifest.replace(PathBuf::from(path)).is_some() {
                    eprintln!("error: `--manifest` may be specified only once");
                    return Err(ExitCode::from(2));
                }
            }
            "--refl" => {
                let Some(argument) = args.next() else {
                    eprintln!("error: `--refl` needs an argument");
                    return Err(ExitCode::from(2));
                };
                command.reflection_arguments.push(argument);
            }
            "--workers" => {
                let Some(count) = args.next() else {
                    eprintln!("error: `--workers` needs a non-negative integer");
                    return Err(ExitCode::from(2));
                };
                let worker_threads = parse_worker_count(&count, "--workers")?;
                if command.worker_threads.replace(worker_threads).is_some() {
                    eprintln!("error: `--workers` may be specified only once");
                    return Err(ExitCode::from(2));
                }
            }
            option if script_extension(option).is_some() => {
                let extension = script_extension(option).expect("checked above").to_owned();
                let Some(body) = args.next() else {
                    eprintln!("error: `{option}` needs a script body");
                    return Err(ExitCode::from(2));
                };
                command.inputs.push(ModuleInput::script(extension, body));
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

fn parse_worker_count(value: &str, source: &str) -> Result<usize, ExitCode> {
    value.parse::<usize>().map_err(|_| {
        eprintln!("error: `{source}` requires a non-negative integer, got `{value}`");
        ExitCode::from(2)
    })
}

fn configured_worker_count(command_line: Option<usize>) -> Result<usize, ExitCode> {
    if let Some(worker_threads) = command_line {
        return Ok(worker_threads);
    }
    let Some(value) = env::var_os("GLAM_WORKERS") else {
        return Ok(0);
    };
    let Some(value) = value.to_str() else {
        eprintln!("error: `GLAM_WORKERS` must be a non-negative integer");
        return Err(ExitCode::from(2));
    };
    parse_worker_count(value, "GLAM_WORKERS")
}

fn script_extension(option: &str) -> Option<&str> {
    option
        .strip_prefix("--script.")
        .or_else(|| option.strip_prefix("-s."))
        .filter(|extension| !extension.is_empty())
}

fn process_reflection_environment(reflection_arguments: Vec<String>) -> Value {
    fn os_value(value: std::ffi::OsString) -> Value {
        Value::binary(value.as_encoded_bytes().to_vec())
    }

    let variables = Value::dictionary(env::vars_os().map(|(name, value)| {
        (
            Value::binary(name.as_encoded_bytes().to_vec()),
            os_value(value),
        )
    }))
    .expect("OS environment names must be keyable binary values");
    let arguments = Value::list(env::args_os().map(os_value));
    let reflection_arguments = Value::list(reflection_arguments.into_iter().map(Value::text));
    Value::record([(
        "process",
        Value::record([
            ("args", arguments),
            ("env", variables),
            ("refl_args", reflection_arguments),
        ]),
    )])
}

fn finish_local_files(
    files: &FileSourceSystem,
    manifest: Option<&Path>,
    diagnostics: &DiagnosticBus,
) -> bool {
    let mut failed = false;
    if let Err(warning) = files.verify_unchanged() {
        diagnostics.publish(Diagnostic::new(Severity::Warning, warning.to_string()));
    }
    if let Some(path) = manifest
        && let Err(error) = files.write_manifest(path)
    {
        failed = true;
        diagnostics.publish(Diagnostic::new(Severity::Error, error.to_string()));
    }
    failed
}

fn assemble_inputs(command: AssemblyCommand) -> ExitCode {
    let AssemblyCommand {
        inputs,
        arguments,
        reflection_arguments,
        manifest,
        worker_threads,
    } = command;
    let worker_threads = match configured_worker_count(worker_threads) {
        Ok(worker_threads) => worker_threads,
        Err(exit_code) => return exit_code,
    };
    let local_files = FileSourceSystem::default();
    let runtime = match EvaluationRuntime::new(worker_threads) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    let conflict_analysis: Arc<dyn ConflictAnalysisStrategy> = Arc::new(ExactConflictAnalysis);
    let log_host = Arc::new(LogHost::with_conflict_analysis(conflict_analysis.clone()));
    let assembler = Assembler::builder()
        .source_system(local_files.clone())
        .evaluation_runtime(runtime)
        .conflict_analysis(conflict_analysis)
        .diagnostic_subscriber(log_host.clone())
        .reflection_environment(|_| Ok(process_reflection_environment(reflection_arguments)))
        .expect("main's reflection environment must be a dictionary")
        .build()
        .expect("main's assembler configuration must be valid");
    let assembler_diagnostics = assembler.diagnostic_bus();
    let configuration = match load_configuration(&assembler) {
        Ok(configuration) => configuration,
        Err(error) => {
            assembler_diagnostics.publish(Diagnostic::new(Severity::Error, error.to_string()));
            finish_local_files(&local_files, manifest.as_deref(), &assembler_diagnostics);
            log_host.close_input();
            log_host.drain_default(&DefaultLogger::new(assembler.clone()));
            return ExitCode::from(1);
        }
    };
    let logger = start_logger(&assembler, &configuration.value, log_host.clone());
    let result = assemble(&assembler, inputs, arguments, configuration.environment);
    let mut operation_failed = false;
    match result {
        Ok(bytes) => {
            if let Err(error) = io::stdout().write_all(&bytes) {
                operation_failed = true;
                assembler_diagnostics.publish(Diagnostic::new(
                    Severity::Error,
                    format!("could not write stdout: {error}"),
                ));
            }
        }
        Err(error) => {
            operation_failed = true;
            assembler_diagnostics.publish(Diagnostic::new(Severity::Error, error.to_string()));
        }
    }

    report_reasoning(&assembler_diagnostics, &assembler.drain_reasoning());
    operation_failed |=
        finish_local_files(&local_files, manifest.as_deref(), &assembler_diagnostics);
    log_host.close_input();
    let LoggerRun {
        thread: logger_thread,
        diagnostics: logger_diagnostics,
    } = logger;
    logger_thread.join().expect("logger task should not panic");
    log_host.cancel();

    if operation_failed
        || assembler_diagnostics.counts().errors() > 0
        || logger_diagnostics.counts().errors() > 0
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn report_reasoning(diagnostics: &DiagnosticBus, report: &ReasoningReport) {
    for failure in report.failures() {
        diagnostics.publish(Diagnostic::new(
            Severity::Error,
            format!(
                "reflection task {} failed: {}",
                failure.task_id(),
                failure.message()
            ),
        ));
    }
    if report.status() != ReasoningStatus::Deadlocked {
        return;
    }

    let mut message = format!(
        "reflection scheduler deadlocked with {} unfinished task{}",
        report.unfinished().len(),
        if report.unfinished().len() == 1 {
            ""
        } else {
            "s"
        }
    );
    for task in report.unfinished() {
        let detail = match task.state() {
            ReasoningTaskState::Blocked => match (
                task.waiting_on_task(),
                task.observed_generation(),
                task.wait_id(),
            ) {
                (Some(dependency), Some(generation), _) => {
                    format!("waits on task {dependency} and shared-state generation {generation}")
                }
                (Some(dependency), None, _) => format!("waits on task {dependency}"),
                (None, Some(generation), Some(wait)) => {
                    format!("waits on token {wait} and shared-state generation {generation}")
                }
                (None, Some(generation), None) => {
                    format!("waits on shared-state generation {generation}")
                }
                (None, None, Some(wait)) => format!("waits on token {wait}"),
                (None, None, None) => "is blocked without a wake condition".to_owned(),
            },
            state => format!("remains in anomalous {state:?} state"),
        };
        message.push_str(&format!("\ntask {} {detail}", task.task_id()));
    }
    diagnostics.publish(Diagnostic::new(Severity::Error, message));
}

fn assemble(
    assembler: &Assembler,
    inputs: Vec<ModuleInput>,
    cli_args: Vec<String>,
    environment: Value,
) -> Result<Bytes, Error> {
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

struct LoadedConfiguration {
    value: Value,
    environment: Value,
}

fn load_configuration(assembler: &Assembler) -> Result<LoadedConfiguration, Error> {
    let default_environment = empty_environment_object();
    let initial_definitions = Value::record([("env", default_environment.clone())]);
    let module = assembler
        .module(["configuration"])
        .initial_definitions(initial_definitions)
        .inputs(configuration_paths().into_iter().map(ModuleInput::file))
        .build()?;

    let environment = match assembler.get(module.value(), "conf.env") {
        Ok(environment) if !environment.is_undefined() => assembler.evaluate(&environment)?,
        Ok(_) | Err(_) => default_environment,
    };
    Ok(LoadedConfiguration {
        value: module.into_value(),
        environment,
    })
}

fn start_logger(assembler: &Assembler, configuration: &Value, input: Arc<LogHost>) -> LoggerRun {
    let logger = Arc::new(DefaultLogger::new(assembler.clone()));
    let diagnostics = DiagnosticBus::new();
    let subscription = diagnostics.subscribe(logger.clone());
    let host = Arc::new(LoggerTaskHost::new(
        input.clone(),
        diagnostics.clone(),
        assembler.reflection_environment_for_role("logger"),
    ));
    let effect_assembler = assembler.clone();
    let evaluation_runtime = assembler.evaluation_runtime();
    let custom = assembler
        .get(configuration, "conf.log")
        .ok()
        .filter(|logger| !logger.is_undefined());
    let task_diagnostics = diagnostics.clone();
    let thread = thread::spawn(move || {
        let _subscription = subscription;
        if let Some(custom) = custom {
            let reflection_host: Arc<dyn ReflectionHost<ReflectionEffects>> = host.clone();
            match EffectRun::new(&custom, MainEffects::new(effect_assembler), host.clone())
                .with_runtime(&evaluation_runtime)
                .with_reflection_children(reflection_host)
                .requiring_unit_result()
                .run()
            {
                Ok(TaskOutcome::Complete(_)) => {}
                Ok(TaskOutcome::Cancelled) => {
                    task_diagnostics.publish(Diagnostic::new(
                        Severity::Error,
                        "configured logger remained blocked after the log stream closed",
                    ));
                }
                Err(error) => {
                    task_diagnostics.publish(Diagnostic::new(
                        Severity::Error,
                        format!("configured logger failed: {error}"),
                    ));
                }
            }
        }
        input.drain_default(logger.as_ref());
    });
    LoggerRun {
        thread,
        diagnostics,
    }
}

struct LoggerRun {
    thread: thread::JoinHandle<()>,
    diagnostics: DiagnosticBus,
}

#[derive(Clone)]
struct MainEffects {
    assembler: Assembler,
}

impl MainEffects {
    fn new(assembler: Assembler) -> Self {
        Self { assembler }
    }
}

#[derive(Clone)]
enum MainRequest {
    Reflection(ReflectionRequest),
    LogStatus,
    ReadLog,
    WriteStderr,
}

#[derive(Clone)]
struct MainSnapshot {
    diagnostics: Arc<[DiagnosticEvent]>,
    input_closed: bool,
    input_revision: u64,
}

#[derive(Clone, Default)]
struct MainJournal {
    reflection: ReflectionJournal,
    consumed_diagnostics: usize,
    stderr: Vec<Bytes>,
    observed_input: bool,
}

impl ReflectionTransaction for MainJournal {
    fn reflection_journal(&mut self) -> &mut ReflectionJournal {
        &mut self.reflection
    }
}

impl TaskSpecialization for MainEffects {
    type Host = LoggerTaskHost;
    type Request = MainRequest;
    type Snapshot = MainSnapshot;
    type Journal = MainJournal;

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        reflection_request_specs()
            .into_iter()
            .map(|request| request.map_request(MainRequest::Reflection))
            .chain([
                EffectRequestSpec::new(
                    "log_status",
                    ["glam_cli", "v0", "request", "log_status"],
                    0,
                    MainRequest::LogStatus,
                ),
                EffectRequestSpec::new(
                    "read_log",
                    ["glam_cli", "v0", "request", "read_log"],
                    0,
                    MainRequest::ReadLog,
                ),
                EffectRequestSpec::new(
                    "write_stderr",
                    ["glam_cli", "v0", "request", "write_stderr"],
                    1,
                    MainRequest::WriteStderr,
                ),
            ])
            .collect()
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, glam::reflection::TaskError> {
        match request {
            MainRequest::Reflection(request) => {
                handle_reflection_request(request, arguments, context)
            }
            MainRequest::LogStatus => log_status(arguments, context),
            MainRequest::ReadLog => read_log(context),
            MainRequest::WriteStderr => {
                let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
                    glam::reflection::TaskError::new(
                        "`.write_stderr` received the wrong number of arguments",
                    )
                })?;
                let bytes = self
                    .assembler
                    .to_binary(&value)
                    .map_err(|error| glam::reflection::TaskError::new(error.to_string()))?;
                if let Some(mut transaction) = context.transaction() {
                    transaction.parts().1.stderr.push(bytes);
                } else {
                    context.host().input.write_stderr(bytes);
                    context.committed();
                }
                Ok(RequestResult::ReturnUnit)
            }
        }
    }
}

fn log_status(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MainEffects>,
) -> Result<RequestResult, glam::reflection::TaskError> {
    if !arguments.is_empty() {
        return Err(glam::reflection::TaskError::new(
            "`.log_status` received the wrong number of arguments",
        ));
    }
    let (generation, input_closed) = if let Some(generation) = context.transaction_generation() {
        let mut transaction = context
            .transaction()
            .expect("checked active reflection transaction");
        let (snapshot, journal) = transaction.parts();
        journal.observed_input = true;
        (generation, snapshot.input_closed)
    } else {
        let snapshot = <LoggerTaskHost as TaskHost<MainEffects>>::snapshot(context.host());
        (snapshot.generation(), snapshot.extra().input_closed)
    };
    context.observe_host_generation(generation);
    Ok(RequestResult::Return(Value::atom_from_text(
        if input_closed { "closed" } else { "open" },
    )))
}

fn read_log(
    context: &mut RequestContext<'_, MainEffects>,
) -> Result<RequestResult, glam::reflection::TaskError> {
    if let Some(generation) = context.transaction_generation() {
        context.observe_host_generation(generation);
        let mut transaction = context
            .transaction()
            .expect("checked active reflection transaction");
        let (snapshot, journal) = transaction.parts();
        journal.observed_input = true;
        if let Some(diagnostic) = snapshot.diagnostics.get(journal.consumed_diagnostics) {
            journal.consumed_diagnostics += 1;
            return diagnostic
                .enrich()
                .map(RequestResult::Return)
                .map_err(|error| glam::reflection::TaskError::new(error.to_string()));
        }
        // Queue reads observe only the host snapshot. Journaled writes remain
        // invisible until commit, just as writes from concurrent tasks do.
        return Ok(RequestResult::Fail);
    }

    loop {
        let snapshot = <LoggerTaskHost as TaskHost<MainEffects>>::snapshot(context.host());
        context.observe_host_generation(snapshot.generation());
        let Some(diagnostic) = snapshot.extra().diagnostics.first() else {
            return Ok(RequestResult::Fail);
        };
        let value = diagnostic
            .enrich()
            .map_err(|error| glam::reflection::TaskError::new(error.to_string()))?;
        let commit = TaskCommit::new(
            glam::reflection::StoreJournal::new(snapshot.store().clone()),
            snapshot.extra().clone(),
            MainJournal {
                reflection: ReflectionJournal::default(),
                consumed_diagnostics: 1,
                stderr: Vec::new(),
                observed_input: true,
            },
        );
        match <LoggerTaskHost as TaskHost<MainEffects>>::commit(context.host(), commit) {
            CommitResult::Committed => {
                context.committed();
                return Ok(RequestResult::Return(value));
            }
            CommitResult::Conflict => {}
            CommitResult::MissingVolume(volume) => {
                return Err(glam::reflection::TaskError::new(format!(
                    "reflection volume {} was revoked before its edits committed",
                    volume.get()
                )));
            }
            CommitResult::Closed => return Ok(RequestResult::Cancelled),
        }
    }
}

struct LogHost {
    state: Mutex<LogHostState>,
    changed: Condvar,
}

/// Capabilities and mutable state belonging to the logger's evaluation
/// session. Incoming assembler diagnostics remain in `input`; diagnostics
/// emitted by this session go only to its diagnostic bus.
struct LoggerTaskHost {
    input: Arc<LogHost>,
    diagnostics: DiagnosticBus,
    reflection_environment: Value,
}

impl LoggerTaskHost {
    fn new(input: Arc<LogHost>, diagnostics: DiagnosticBus, reflection_environment: Value) -> Self {
        Self {
            input,
            diagnostics,
            reflection_environment,
        }
    }

    fn emit_output(&self, diagnostic: Diagnostic) {
        self.diagnostics.publish(diagnostic);
    }
}

struct LogHostState {
    wake_generation: u64,
    input_revision: u64,
    store: ReflectionStore,
    diagnostics: VecDeque<DiagnosticEvent>,
    stderr: VecDeque<Bytes>,
    input_closed: bool,
    cancelled: bool,
}

impl LogHost {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_conflict_analysis(Arc::new(glam::reflection::ExactConflictAnalysis))
    }

    fn with_conflict_analysis(strategy: Arc<dyn ConflictAnalysisStrategy>) -> Self {
        Self {
            state: Mutex::new(LogHostState {
                wake_generation: 1,
                input_revision: 0,
                store: ReflectionStore::new(strategy),
                diagnostics: VecDeque::new(),
                stderr: VecDeque::new(),
                input_closed: false,
                cancelled: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn close_input(&self) {
        let mut state = self
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        state.input_closed = true;
        state.input_revision = state.input_revision.wrapping_add(1);
        state.wake_generation = state.wake_generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        state.cancelled = true;
        state.input_revision = state.input_revision.wrapping_add(1);
        state.wake_generation = state.wake_generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn drain_default(&self, logger: &DefaultLogger) {
        while let Some(diagnostic) = self.take_diagnostic() {
            logger.emit(&diagnostic);
        }
        self.flush_stderr();
    }

    fn take_diagnostic(&self) -> Option<DiagnosticEvent> {
        let mut state = self
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        loop {
            if let Some(diagnostic) = state.diagnostics.pop_front() {
                state.input_revision = state.input_revision.wrapping_add(1);
                state.wake_generation = state.wake_generation.wrapping_add(1);
                self.changed.notify_all();
                return Some(diagnostic);
            }
            if state.input_closed || state.cancelled {
                return None;
            }
            state = self
                .changed
                .wait(state)
                .expect("log host mutex should not be poisoned");
        }
    }

    fn flush_stderr(&self) {
        let output = {
            let mut state = self
                .state
                .lock()
                .expect("log host mutex should not be poisoned");
            state.stderr.drain(..).collect::<Vec<_>>()
        };
        let mut stderr = io::stderr().lock();
        for bytes in output {
            let _ = stderr.write_all(&bytes);
        }
    }

    fn write_stderr(&self, bytes: Bytes) {
        self.state
            .lock()
            .expect("log host mutex should not be poisoned")
            .stderr
            .push_back(bytes);
        self.flush_stderr();
    }

    fn push_diagnostic(&self, state: &mut LogHostState, event: DiagnosticEvent) {
        state.diagnostics.push_back(event);
    }
}

impl DiagnosticSubscriber for LogHost {
    fn receive(&self, event: DiagnosticEvent) {
        let mut state = self
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        self.push_diagnostic(&mut state, event);
        state.input_revision = state.input_revision.wrapping_add(1);
        state.wake_generation = state.wake_generation.wrapping_add(1);
        self.changed.notify_all();
    }
}

impl TaskEnvironment for LoggerTaskHost {
    fn reflection_environment(&self) -> Value {
        self.reflection_environment.clone()
    }
}

impl ReflectionServices for LoggerTaskHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        self.emit_output(diagnostic);
    }

    fn complete_query(&self, handle: &Arc<glam::reflection::EvaluationQueryHandle>, result: Value) {
        let mut state = self
            .input
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        if state.store.complete_query(handle, result) {
            state.wake_generation = state.wake_generation.wrapping_add(1);
            self.input.changed.notify_all();
        }
    }
}

impl TaskHost<MainEffects> for LoggerTaskHost {
    fn snapshot(&self) -> HostSnapshot<MainEffects> {
        let state = self
            .input
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        HostSnapshot::new(
            state.wake_generation,
            state.store.snapshot(),
            MainSnapshot {
                diagnostics: Arc::from(state.diagnostics.iter().cloned().collect::<Vec<_>>()),
                input_closed: state.input_closed,
                input_revision: state.input_revision,
            },
        )
    }

    fn commit(&self, commit: TaskCommit<MainEffects>) -> CommitResult {
        let (store, snapshot, journal) = commit.into_parts();
        let diagnostics = {
            let mut state = self
                .input
                .state
                .lock()
                .expect("log host mutex should not be poisoned");
            if (journal.observed_input && state.input_revision != snapshot.input_revision)
                || state.diagnostics.len() < journal.consumed_diagnostics
            {
                return CommitResult::Conflict;
            }
            match state.store.try_commit(&store) {
                glam::reflection::StoreCommitResult::Committed => {}
                glam::reflection::StoreCommitResult::Conflict => {
                    return CommitResult::Conflict;
                }
                glam::reflection::StoreCommitResult::MissingVolume(volume) => {
                    return CommitResult::MissingVolume(volume);
                }
            }
            state.diagnostics.drain(..journal.consumed_diagnostics);
            if journal.consumed_diagnostics != 0 {
                state.input_revision = state.input_revision.wrapping_add(1);
            }
            state.stderr.extend(journal.stderr.iter().cloned());
            state.wake_generation = state.wake_generation.wrapping_add(1);
            self.input.changed.notify_all();
            journal.reflection.diagnostics().to_vec()
        };
        for diagnostic in diagnostics {
            self.emit_output(diagnostic);
        }
        journal.reflection.commit_updates();
        self.input.flush_stderr();
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        let mut state = self
            .input
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        if state.wake_generation != observed_generation {
            return true;
        }
        while state.wake_generation == observed_generation
            && !state.cancelled
            && !(state.input_closed && state.diagnostics.is_empty())
        {
            state = self
                .input
                .changed
                .wait(state)
                .expect("log host mutex should not be poisoned");
        }
        state.wake_generation != observed_generation
    }
}

impl TaskHost<ReflectionEffects> for LoggerTaskHost {
    fn snapshot(&self) -> HostSnapshot<ReflectionEffects> {
        let state = self
            .input
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        HostSnapshot::new(state.wake_generation, state.store.snapshot(), ())
    }

    fn commit(&self, commit: TaskCommit<ReflectionEffects>) -> CommitResult {
        let (store, _snapshot, journal) = commit.into_parts();
        let diagnostics = {
            let mut state = self
                .input
                .state
                .lock()
                .expect("log host mutex should not be poisoned");
            match state.store.try_commit(&store) {
                glam::reflection::StoreCommitResult::Committed => {}
                glam::reflection::StoreCommitResult::Conflict => {
                    return CommitResult::Conflict;
                }
                glam::reflection::StoreCommitResult::MissingVolume(volume) => {
                    return CommitResult::MissingVolume(volume);
                }
            }
            state.wake_generation = state.wake_generation.wrapping_add(1);
            self.input.changed.notify_all();
            journal.diagnostics().to_vec()
        };
        for diagnostic in diagnostics {
            self.emit_output(diagnostic);
        }
        journal.commit_updates();
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        <LoggerTaskHost as TaskHost<MainEffects>>::wait_for_change(self, observed_generation)
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
    formatter: Value,
    working_directory: PathBuf,
}

impl DefaultLogger {
    const AUTO_INDENT: usize = 2;

    fn new(evaluator: Assembler) -> Self {
        let formatter = evaluator.default_diagnostic_formatter();
        Self {
            evaluator,
            formatter,
            working_directory: env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    fn emit(&self, diagnostic: &Diagnostic) {
        let terminal = TerminalContext::snapshot();
        let updates = self.viewer_updates(diagnostic, &terminal);
        let rendered = diagnostic
            .enrich_with(updates)
            .and_then(|message| self.evaluator.apply(&self.formatter, [message]))
            .and_then(|rendered| self.evaluator.to_binary(&rendered))
            .unwrap_or_else(|_| {
                Bytes::from(self.render(diagnostic, diagnostic.message(), terminal.color))
            });

        let _ = io::stderr().lock().write_all(&rendered);
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
            ("indent", Value::text(" ".repeat(Self::AUTO_INDENT))),
            ("location", Value::text(self.location(diagnostic))),
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
        let mut rendered = format!("{}{severity}: ", self.location(diagnostic));
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

    fn location(&self, diagnostic: &Diagnostic) -> String {
        match (diagnostic.source(), diagnostic.line()) {
            (Some(source), Some(line)) => {
                format!("{}:{line}: ", self.display_source(Path::new(source)))
            }
            (Some(source), None) => format!("{}: ", self.display_source(Path::new(source))),
            (None, Some(line)) => format!("line {line}: "),
            (None, None) => String::new(),
        }
    }

    fn display_source(&self, source: &Path) -> String {
        source
            .strip_prefix(&self.working_directory)
            .unwrap_or(source)
            .display()
            .to_string()
    }
}

impl DiagnosticSubscriber for DefaultLogger {
    fn receive(&self, event: DiagnosticEvent) {
        DefaultLogger::emit(self, &event);
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
            [--manifest <PATH>]
            [--refl <ARG>]...
            [--workers <N>]
       glam --parse <PATH>
       glam --check_manifest <PATH> [--quiet]
       glam --help
       glam --version

Assembly inputs are applied as mixins; earlier inputs override later inputs.
--manifest records every local input path and its SHA-256 digest.
--check_manifest verifies every local file recorded by a manifest.
--quiet suppresses changed-file output from --check_manifest.
--refl appends an argument visible only as reflection environment process.refl_args.
--workers sets the shared background evaluator thread count; zero disables sparks.
GLAM_WORKERS provides the default worker count when --workers is absent.
Configuration is loaded from GLAM_CONF as an OS path-list, or from the user config/default fixture.
Bare arguments are reserved for configured `conf.cli` rewriting.
";

    print!("{HELP}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::SourceSystem;

    #[test]
    fn final_local_file_change_is_only_a_warning() {
        let directory =
            env::temp_dir().join(format!("glam-final-file-warning-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("test directory should be created");
        let path = directory.join("input.g");
        fs::write(&path, "used").expect("test input should be written");
        let files = FileSourceSystem::default();
        files
            .load_top_level(&path)
            .expect("assembly read should succeed");
        fs::write(&path, "later edit").expect("test input should be changed");
        let diagnostics = DiagnosticBus::new();
        let queue = Arc::new(LogHost::new());
        let _subscription = diagnostics.subscribe(queue.clone());

        assert!(!finish_local_files(&files, None, &diagnostics));
        let warning = queue
            .take_diagnostic()
            .expect("final file change should emit a diagnostic");
        assert_eq!(warning.severity(), Severity::Warning);
        assert_eq!(diagnostics.counts().warnings(), 1);
        assert_eq!(diagnostics.counts().errors(), 0);
    }

    #[test]
    fn glam_default_formatter_renders_location_severity_and_continuation_lines() {
        let evaluator = Assembler::default();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(Severity::Warning, "first\nsecond\n\nfourth")
            .with_source_location("/work/src/test.g", 4);
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: None,
            language: None,
        };
        let enriched = diagnostic
            .enrich_with(logger.viewer_updates(&diagnostic, &terminal))
            .expect("terminal viewer metadata should mix into a diagnostic");
        let rendered_value = logger
            .evaluator
            .apply(&logger.formatter, [enriched])
            .expect("the closed Glam formatter should apply");
        let rendered_value = logger
            .evaluator
            .evaluate(&rendered_value)
            .expect("the formatter result should evaluate");
        let rendered = logger
            .evaluator
            .to_binary(&rendered_value)
            .expect("the closed Glam formatter should return bytes");

        assert_eq!(
            rendered,
            Bytes::from_static(b"src/test.g:4: warning: first\n  second\n  \n  fourth\n")
        );
    }

    #[test]
    fn glam_default_formatter_applies_terminal_color_policy() {
        let evaluator = Assembler::default();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(Severity::Error, "broken");
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::Ansi256,
            term: None,
            language: None,
        };
        let enriched = diagnostic
            .enrich_with(logger.viewer_updates(&diagnostic, &terminal))
            .expect("terminal viewer metadata should mix into a diagnostic");
        let rendered = logger
            .evaluator
            .apply(&logger.formatter, [enriched])
            .and_then(|value| logger.evaluator.to_binary(&value))
            .expect("the closed Glam formatter should return colored bytes");

        assert_eq!(
            rendered,
            Bytes::from_static(b"\x1b[31merror\x1b[0m: broken\n")
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

    #[test]
    fn bus_error_count_survives_absent_subscribers_and_queue_reads() {
        let diagnostics = DiagnosticBus::new();
        diagnostics.publish(Diagnostic::new(Severity::Error, "dropped"));
        assert_eq!(diagnostics.counts().errors(), 1);

        let retained = Arc::new(LogHost::new());
        let _retained_subscription = diagnostics.subscribe(retained.clone());
        diagnostics.publish(Diagnostic::new(Severity::Error, "retained"));
        assert!(retained.take_diagnostic().is_some());
        assert_eq!(diagnostics.counts().errors(), 2);
        retained.close_input();
    }

    #[test]
    fn logger_session_output_is_separate_from_assembler_input() {
        let input = Arc::new(LogHost::new());
        let diagnostics = DiagnosticBus::new();
        let output = Arc::new(LogHost::new());
        let _subscription = diagnostics.subscribe(output.clone());
        let host = LoggerTaskHost::new(
            input.clone(),
            diagnostics.clone(),
            Assembler::default().reflection_environment_for_role("logger"),
        );

        <LoggerTaskHost as ReflectionServices>::emit_diagnostic(
            &host,
            Diagnostic::new(Severity::Error, "session output"),
        );

        assert!(
            input
                .state
                .lock()
                .expect("log host mutex should not be poisoned")
                .diagnostics
                .is_empty()
        );
        let output = output
            .take_diagnostic()
            .expect("logger output bus should publish the diagnostic");
        assert_eq!(output.message(), "session output");
        assert_eq!(diagnostics.counts().errors(), 1);
    }
}
