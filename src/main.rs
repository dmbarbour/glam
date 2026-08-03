use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;

use bytes::Bytes;
use glam::cli::{
    CliArguments, CommandPlan, CommandPlanParts, CompletionRoute, HELP_TEXT, TopLevelCommand,
    builtin_completion_script, complete_basic, complete_configured, dispatch_bootstrap,
    expand_configured, format_completion_replacements, format_configured_arguments,
    format_parse_summary, parse_worker_count, route_completion,
};
use glam::reflection::{
    CommitResult, EffectRequestSpec, EffectRun, HostSnapshot, ReflectionJournal, ReflectionRequest,
    ReflectionServices, ReflectionTransaction, RequestContext, RequestResult, TaskCommit,
    TaskEnvironment, TaskHost, TaskOutcome, TaskSpecialization, handle_reflection_request,
    reflection_request_specs,
};
use glam::{
    Assembler, Diagnostic, DiagnosticBus, DiagnosticEvent, DiagnosticIngress, DiagnosticSubscriber,
    Error, EvaluationRuntime, FileSourceSystem, ModuleInput, PromiseResolver, ReasoningReport,
    ReasoningStatus, ReasoningTaskState, RuntimeDeliveryOutcome, RuntimeEventJournal,
    RuntimeInputReader, RuntimeLoggerSnapshot, RuntimeOutputDelivery, RuntimeOutputWriter,
    Severity, Value, check_local_manifest, inspect_g_source,
};

fn main() -> ExitCode {
    let command = match dispatch_bootstrap(env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };

    match command {
        TopLevelCommand::Help => {
            print!("{HELP_TEXT}");
            ExitCode::SUCCESS
        }
        TopLevelCommand::Version => {
            println!("glam {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        TopLevelCommand::InspectGSource { path, verbosity } => {
            inspect_g_source_command(&path, verbosity)
        }
        TopLevelCommand::CheckManifest { path, quiet } => check_manifest_command(&path, quiet),
        TopLevelCommand::Assembly(plan) => assemble_inputs(plan),
        TopLevelCommand::ConfiguredCli(arguments) => configured_cli(arguments, None),
        TopLevelCommand::InspectConfiguredCli {
            arguments,
            nul_terminated,
        } => configured_cli(arguments, Some(nul_terminated)),
        TopLevelCommand::Complete(request) => completion_command(request),
        TopLevelCommand::CompletionScript {
            name,
            cli_arguments,
        } => completion_script_command(&name, cli_arguments),
    }
}

fn completion_command(request: glam::cli::CompletionRequest) -> ExitCode {
    match route_completion(request) {
        CompletionRoute::Basic(request) => write_completion(&complete_basic(&request)).map_or_else(
            |error| {
                eprintln!("error: {error}");
                ExitCode::from(1)
            },
            |()| ExitCode::SUCCESS,
        ),
        CompletionRoute::Configured(request) => configured_completion(request),
    }
}

fn configured_completion(request: glam::cli::CompletionRequest) -> ExitCode {
    let cli_arguments = CliArguments::from_args(request.arguments().iter().cloned());
    let prepared = match prepare_assembly(cli_arguments, None, None) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let completion =
        match complete_configured(&prepared.assembler, &prepared.configuration.value, request) {
            Ok(completion) => completion,
            Err(error) => {
                for diagnostic in error.diagnostics() {
                    prepared
                        .assembler
                        .diagnostic_bus()
                        .publish(diagnostic.clone());
                }
                prepared
                    .assembler
                    .diagnostic_bus()
                    .publish(error.diagnostic());
                return finish_without_logger(prepared, None, true);
            }
        };
    for diagnostic in completion.diagnostics() {
        prepared
            .assembler
            .diagnostic_bus()
            .publish(diagnostic.clone());
    }
    let failed = write_completion(&completion).is_err_and(|error| {
        prepared
            .assembler
            .diagnostic_bus()
            .publish(Diagnostic::new(Severity::Error, error));
        true
    });
    finish_without_logger(prepared, None, failed)
}

fn write_completion(completion: &glam::cli::CliCompletion) -> Result<(), String> {
    io::stdout()
        .write_all(&format_completion_replacements(completion))
        .map_err(|error| format!("could not write completions to stdout: {error}"))
}

fn completion_script_command(name: &std::ffi::OsStr, cli_arguments: CliArguments) -> ExitCode {
    let Some(name) = name
        .to_str()
        .filter(|name| !name.is_empty() && !name.contains('.'))
    else {
        eprintln!("error: completion script binding name must be nonempty UTF-8 without `.`");
        return ExitCode::from(2);
    };
    let initial_environment = Some((
        argument_values(cli_arguments.args()),
        Value::list(std::iter::empty()),
    ));
    let prepared = match prepare_assembly(cli_arguments, None, initial_environment) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let path = format!("conf.completion_script.{name}");
    let configured = prepared
        .assembler
        .get(&prepared.configuration.value, &path)
        .ok()
        .filter(|value| !value.is_undefined());
    let output: Result<Vec<u8>, String> = match configured {
        Some(function) => configured_completion_script(&prepared.assembler, &function),
        None => builtin_completion_script(name)
            .map(|script| script.as_bytes().to_vec())
            .ok_or_else(|| format!("unknown completion script binding `{name}`")),
    };
    let failed = match output {
        Ok(output) => io::stdout().write_all(&output).is_err_and(|error| {
            prepared.assembler.diagnostic_bus().publish(Diagnostic::new(
                Severity::Error,
                format!("could not write completion script to stdout: {error}"),
            ));
            true
        }),
        Err(error) => {
            prepared
                .assembler
                .diagnostic_bus()
                .publish(Diagnostic::new(Severity::Error, error));
            true
        }
    };
    finish_without_logger(prepared, None, failed)
}

fn configured_completion_script(
    assembler: &Assembler,
    function: &Value,
) -> Result<Vec<u8>, String> {
    let executable = env::args_os().next().unwrap_or_else(|| "glam".into());
    let context = Value::record([
        (
            "executable",
            Value::binary(executable.as_encoded_bytes().to_vec()),
        ),
        ("protocol", Value::text("v0")),
        ("request", Value::text("--completions")),
    ]);
    assembler
        .apply(function, [context])
        .and_then(|value| assembler.to_binary(&value))
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
}

fn check_manifest_command(manifest: &Path, quiet: bool) -> ExitCode {
    match check_local_manifest(manifest) {
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

fn configured_worker_count(command_line: Option<usize>) -> Result<usize, ExitCode> {
    if let Some(worker_threads) = command_line {
        return Ok(worker_threads);
    }
    let Some(value) = env::var_os("GLAM_WORKERS") else {
        return Ok(0);
    };
    parse_worker_count(&value, "GLAM_WORKERS").map_err(|error| {
        eprintln!("error: {error}");
        ExitCode::from(2)
    })
}

fn process_reflection_environment(
    reflection_arguments: Value,
    process_arguments: Value,
    cli_arguments: CliArguments,
) -> Value {
    fn os_value(value: &std::ffi::OsStr) -> Value {
        Value::binary(value.as_encoded_bytes().to_vec())
    }

    let variables = Value::dictionary(env::vars_os().map(|(name, value)| {
        (
            Value::binary(name.as_encoded_bytes().to_vec()),
            os_value(&value),
        )
    }))
    .expect("OS environment names must be keyable binary values");
    let cli_arguments = Value::list(
        cli_arguments
            .args()
            .iter()
            .map(|argument| os_value(argument)),
    );
    Value::record([(
        "process",
        Value::record([
            ("args", process_arguments),
            ("env", variables),
            ("refl_args", reflection_arguments),
            ("cli", Value::record([("args", cli_arguments)])),
        ]),
    )])
}

fn argument_values(arguments: &[std::ffi::OsString]) -> Value {
    Value::list(
        arguments
            .iter()
            .map(|argument| Value::binary(argument.as_encoded_bytes().to_vec())),
    )
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

fn publish_error(diagnostics: &DiagnosticBus, error: &Error) {
    diagnostics.publish(error.diagnostic().clone());
}

struct PreparedAssembly {
    local_files: FileSourceSystem,
    runtime: EvaluationRuntime,
    log_host: Arc<LogHost>,
    assembler: Assembler,
    configuration: LoadedConfiguration,
    process_args: Option<PromiseResolver>,
    reflection_args: Option<PromiseResolver>,
}

impl PreparedAssembly {
    fn resolve_environment(&mut self, command: &CommandPlanParts) -> Result<(), Error> {
        if let Some(resolver) = self.process_args.take() {
            resolver.resolve(argument_values(&command.process_args))?;
        }
        if let Some(resolver) = self.reflection_args.take() {
            resolver.resolve(argument_values(&command.reflection_args))?;
        }
        Ok(())
    }

    fn fail_environment(&mut self, message: &str) {
        if let Some(resolver) = self.process_args.take() {
            let _ = resolver.fail_message(message);
        }
        if let Some(resolver) = self.reflection_args.take() {
            let _ = resolver.fail_message(message);
        }
    }
}

fn prepare_assembly(
    cli_arguments: CliArguments,
    failure_manifest: Option<&Path>,
    initial_environment: Option<(Value, Value)>,
) -> Result<PreparedAssembly, ExitCode> {
    let local_files = FileSourceSystem::default();
    let runtime = EvaluationRuntime::new(0).expect("a dormant evaluation runtime is valid");
    let diagnostics = DiagnosticBus::for_runtime(&runtime);
    let log_host = Arc::new(LogHost::with_runtime(runtime.clone(), &diagnostics));
    let mut process_args = None;
    let mut reflection_args = None;
    let assembler = Assembler::builder()
        .source_system(local_files.clone())
        .evaluation_runtime(runtime.clone())
        .diagnostic_bus(diagnostics)
        .reflection_environment(|environment| {
            let (process_value, process_resolver) =
                environment.promise("canonical process arguments");
            let (reflection_value, reflection_resolver) =
                environment.promise("canonical reflection arguments");
            process_args = Some(process_resolver);
            reflection_args = Some(reflection_resolver);
            Ok(process_reflection_environment(
                reflection_value,
                process_value,
                cli_arguments,
            ))
        })
        .expect("main's reflection environment must be a dictionary")
        .build()
        .expect("main's assembler configuration must be valid");
    if let Some((process_value, reflection_value)) = initial_environment {
        process_args
            .take()
            .expect("bootstrap process argument resolver should be present")
            .resolve(process_value)
            .expect("fresh bootstrap process argument promise should resolve");
        reflection_args
            .take()
            .expect("bootstrap reflection argument resolver should be present")
            .resolve(reflection_value)
            .expect("fresh bootstrap reflection argument promise should resolve");
    }
    let configuration = match load_configuration(&assembler) {
        Ok(configuration) => configuration,
        Err(error) => {
            let diagnostics = assembler.diagnostic_bus();
            publish_error(&diagnostics, &error);
            finish_local_files(&local_files, failure_manifest, &diagnostics);
            log_host.close_input();
            log_host.drain_default(&DefaultLogger::new(assembler));
            return Err(ExitCode::from(1));
        }
    };
    Ok(PreparedAssembly {
        local_files,
        runtime,
        log_host,
        assembler,
        configuration,
        process_args,
        reflection_args,
    })
}

fn assemble_inputs(command: CommandPlan) -> ExitCode {
    let cli_arguments = command.cli_arguments().clone();
    let manifest = command.manifest().map(Path::to_owned);
    let initial_environment = Some((
        argument_values(command.process_args()),
        argument_values(command.reflection_args()),
    ));
    let prepared = match prepare_assembly(cli_arguments, manifest.as_deref(), initial_environment) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    execute_assembly(prepared, command)
}

fn execute_assembly(mut prepared: PreparedAssembly, command: CommandPlan) -> ExitCode {
    let CommandPlanParts {
        inputs,
        assembly_args,
        reflection_args,
        manifest,
        worker_count,
        process_args,
        cli_arguments,
    } = command.into_parts();
    let command_parts = CommandPlanParts {
        inputs,
        assembly_args,
        reflection_args,
        manifest,
        worker_count,
        process_args,
        cli_arguments,
    };
    if let Err(error) = prepared.resolve_environment(&command_parts) {
        publish_error(&prepared.assembler.diagnostic_bus(), &error);
        return finish_without_logger(prepared, command_parts.manifest.as_deref(), true);
    }
    let worker_threads = match configured_worker_count(worker_count) {
        Ok(worker_threads) => worker_threads,
        Err(exit_code) => {
            finish_without_logger(prepared, command_parts.manifest.as_deref(), true);
            return exit_code;
        }
    };
    if let Err(error) = prepared.runtime.activate_workers(worker_threads) {
        publish_error(&prepared.assembler.diagnostic_bus(), &error);
        return finish_without_logger(prepared, command_parts.manifest.as_deref(), true);
    }
    let CommandPlanParts {
        inputs,
        assembly_args,
        manifest,
        ..
    } = command_parts;
    let PreparedAssembly {
        local_files,
        log_host,
        assembler,
        configuration,
        ..
    } = prepared;
    let assembler_diagnostics = assembler.diagnostic_bus();
    let logger = start_logger(&assembler, &configuration.value, log_host.clone());
    let result = assemble(&assembler, inputs, assembly_args, configuration.environment);
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
            publish_error(&assembler_diagnostics, &error);
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

fn finish_without_logger(
    prepared: PreparedAssembly,
    manifest: Option<&Path>,
    operation_failed: bool,
) -> ExitCode {
    let diagnostics = prepared.assembler.diagnostic_bus();
    report_reasoning(&diagnostics, &prepared.assembler.drain_reasoning());
    let file_failure = finish_local_files(&prepared.local_files, manifest, &diagnostics);
    prepared.log_host.close_input();
    prepared
        .log_host
        .drain_default(&DefaultLogger::new(prepared.assembler));
    if operation_failed || file_failure || diagnostics.counts().errors() != 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn configured_cli(arguments: CliArguments, inspection: Option<bool>) -> ExitCode {
    let mut prepared = match prepare_assembly(arguments.clone(), None, None) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let expansion = match expand_configured(
        &prepared.assembler,
        &prepared.configuration.value,
        arguments,
    ) {
        Ok(expansion) => expansion,
        Err(error) => {
            prepared.fail_environment("configured CLI expansion failed");
            for diagnostic in error.diagnostics() {
                prepared
                    .assembler
                    .diagnostic_bus()
                    .publish(diagnostic.clone());
            }
            prepared
                .assembler
                .diagnostic_bus()
                .publish(error.diagnostic());
            return finish_without_logger(prepared, None, true);
        }
    };
    let (command, diagnostics) = expansion.into_parts();
    for diagnostic in diagnostics {
        prepared.assembler.diagnostic_bus().publish(diagnostic);
    }
    if let Some(nul_terminated) = inspection {
        let parts = command.into_parts();
        if let Err(error) = prepared.resolve_environment(&parts) {
            publish_error(&prepared.assembler.diagnostic_bus(), &error);
            return finish_without_logger(prepared, None, true);
        }
        let output = format_configured_arguments(&parts.process_args, nul_terminated);
        let failed = if let Err(error) = io::stdout().write_all(&output) {
            prepared.assembler.diagnostic_bus().publish(Diagnostic::new(
                Severity::Error,
                format!("could not write configured CLI inspection to stdout: {error}"),
            ));
            true
        } else {
            false
        };
        return finish_without_logger(prepared, None, failed);
    }
    execute_assembly(prepared, command)
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
    let (severity, summary) = match report.status() {
        ReasoningStatus::Complete => return,
        ReasoningStatus::Quiescent => (
            Severity::Warning,
            "reflection scheduler is quiescent on live foreign work",
        ),
        ReasoningStatus::Deadlocked => (Severity::Error, "reflection scheduler deadlocked"),
    };
    if report.unfinished().is_empty() {
        return;
    }

    let mut message = format!(
        "{summary} with {} unfinished task{}",
        report.unfinished().len(),
        if report.unfinished().len() == 1 {
            ""
        } else {
            "s"
        }
    );
    for task in report.unfinished() {
        let mut detail = match task.state() {
            ReasoningTaskState::Blocked => match (
                task.waiting_on_task(),
                task.waiting_on_session(),
                task.observed_generation(),
                task.wait_id(),
            ) {
                (Some(dependency), Some(session), Some(generation), _) => {
                    format!(
                        "waits on task {dependency} in evaluation session {session} and shared-state generation {generation}"
                    )
                }
                (Some(dependency), Some(session), None, _) => {
                    format!("waits on task {dependency} in evaluation session {session}")
                }
                (Some(dependency), None, Some(generation), _) => {
                    format!("waits on task {dependency} and shared-state generation {generation}")
                }
                (Some(dependency), None, None, _) => format!("waits on task {dependency}"),
                (None, _, Some(generation), Some(wait)) => {
                    format!("waits on token {wait} and shared-state generation {generation}")
                }
                (None, _, Some(generation), None) => {
                    format!("waits on shared-state generation {generation}")
                }
                (None, _, None, Some(wait)) => format!("waits on token {wait}"),
                (None, _, None, None) => "is blocked without a wake condition".to_owned(),
            },
            state => format!("remains in anomalous {state:?} state"),
        };
        if let Some(error) = task.blocked_error() {
            detail.push_str(&format!("; retained error: {error}"));
        }
        message.push_str(&format!("\ntask {} {detail}", task.task_id()));
    }
    diagnostics.publish(Diagnostic::new(severity, message));
}

fn assemble(
    assembler: &Assembler,
    inputs: Vec<ModuleInput>,
    cli_args: Vec<std::ffi::OsString>,
    environment: Value,
) -> Result<Bytes, Error> {
    let arguments = Value::list(
        cli_args
            .iter()
            .map(|argument| Value::binary(argument.as_encoded_bytes().to_vec())),
    );
    let initial_definitions = Value::record([
        ("asm", Value::record([("args", arguments)])),
        ("env", environment),
    ]);
    let module = assembler
        .module(["assembly"])
        .initial_definitions(initial_definitions)
        .inputs(inputs)
        .build()?;
    assembler
        .binary_at(module.value(), "asm.result")
        .map_err(|error| error.with_context(&assembler.values(), assembly_result_context()))
}

fn assembly_result_context() -> Value {
    Value::record([(
        "asm",
        Value::record([("result", Value::text("asm.result"))]),
    )])
}

struct LoadedConfiguration {
    value: Value,
    environment: Value,
}

fn load_configuration(assembler: &Assembler) -> Result<LoadedConfiguration, Error> {
    let default_environment = empty_environment_object(&assembler.values());
    let initial_definitions = Value::record([("env", default_environment.clone())]);
    let module = assembler
        .module(["configuration"])
        .initial_definitions(initial_definitions)
        .inputs(configuration_paths().into_iter().map(ModuleInput::file))
        .build()?;

    let environment = match assembler
        .get_optional(module.value(), "conf.env")
        .map_err(|error| {
            error.with_context(&assembler.values(), configuration_entry_context("env"))
        })? {
        Some(environment) if !environment.is_undefined() => {
            assembler.evaluate(&environment).map_err(|error| {
                error.with_context(&assembler.values(), configuration_entry_context("env"))
            })?
        }
        Some(_) | None => default_environment,
    };
    Ok(LoadedConfiguration {
        value: module.into_value(),
        environment,
    })
}

fn start_logger(assembler: &Assembler, configuration: &Value, input: Arc<LogHost>) -> LoggerRun {
    let logger = Arc::new(DefaultLogger::new(assembler.clone()));
    let evaluation_runtime = assembler.evaluation_runtime();
    let diagnostics = DiagnosticBus::for_runtime(&evaluation_runtime);
    let subscription = diagnostics.subscribe(logger.clone());
    let host = Arc::new(LoggerTaskHost::new(
        input.clone(),
        diagnostics.clone(),
        assembler.reflection_environment_for_role("logger"),
    ));
    let effect_assembler = assembler.clone();
    let custom = match assembler.get_optional(configuration, "conf.log") {
        Ok(Some(logger)) if !logger.is_undefined() => Some(logger),
        Ok(Some(_)) | Ok(None) => None,
        Err(error) => {
            diagnostics.publish(
                error
                    .with_context(&assembler.values(), configuration_entry_context("log"))
                    .diagnostic()
                    .clone(),
            );
            None
        }
    };
    let task_diagnostics = diagnostics.clone();
    let task_values = evaluation_runtime.values();
    let thread = thread::spawn(move || {
        let _subscription = subscription;
        if let Some(custom) = custom {
            match EffectRun::new(
                &evaluation_runtime,
                &custom,
                MainEffects::new(effect_assembler),
                host.clone(),
            )
            .asserting_unit_result("configured logger result")
            .requiring_unit_result()
            .run()
            {
                Ok(TaskOutcome::Complete(_)) => {}
                Ok(TaskOutcome::Cancelled) => {
                    task_diagnostics.publish(
                        Diagnostic::new(
                            Severity::Error,
                            "configured logger remained blocked after the log stream closed",
                        )
                        .with_context(configuration_entry_context("log")),
                    );
                }
                Err(error) => {
                    task_diagnostics.publish(
                        error
                            .with_context(configuration_entry_context("log"))
                            .diagnostic(&task_values),
                    );
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

fn configuration_entry_context(entry: &str) -> Value {
    Value::record([("conf", Value::record([("entry", Value::text(entry))]))])
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

type MainSnapshot = RuntimeLoggerSnapshot;

#[derive(Clone, Default)]
struct MainJournal {
    reflection: ReflectionJournal,
    events: Option<RuntimeEventJournal>,
    observed_lifecycle: bool,
}

impl ReflectionTransaction for MainJournal {
    fn reflection_journal(&mut self) -> &mut ReflectionJournal {
        &mut self.reflection
    }
}

fn event_journal<'a>(
    snapshot: &MainSnapshot,
    journal: &'a mut MainJournal,
) -> &'a mut RuntimeEventJournal {
    journal
        .events
        .get_or_insert_with(|| RuntimeEventJournal::new(snapshot.events().clone()))
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
    ) -> Result<RequestResult, glam::reflection::TaskHalt> {
        match request {
            MainRequest::Reflection(request) => {
                handle_reflection_request(request, arguments, context)
            }
            MainRequest::LogStatus => log_status(arguments, context),
            MainRequest::ReadLog => read_log(context),
            MainRequest::WriteStderr => {
                let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
                    glam::reflection::TaskHalt::new(
                        "`.write_stderr` received the wrong number of arguments",
                    )
                })?;
                let bytes = self
                    .assembler
                    .to_binary(&value)
                    .map_err(glam::reflection::TaskHalt::from)?;
                let stderr_writer = context.host().stderr_writer.clone();
                if let Some(mut transaction) = context.transaction() {
                    let (snapshot, journal) = transaction.parts();
                    let value = Value::binary(bytes);
                    event_journal(snapshot, journal)
                        .write(&stderr_writer, value)
                        .map_err(glam::reflection::TaskHalt::from)?;
                } else {
                    context
                        .host()
                        .write_stderr(bytes)
                        .map_err(glam::reflection::TaskHalt::from)?;
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
) -> Result<RequestResult, glam::reflection::TaskHalt> {
    if !arguments.is_empty() {
        return Err(glam::reflection::TaskHalt::new(
            "`.log_status` received the wrong number of arguments",
        ));
    }
    let (generation, input_closed) = if let Some(generation) = context.transaction_generation() {
        let mut transaction = context
            .transaction()
            .expect("checked active reflection transaction");
        let (snapshot, journal) = transaction.parts();
        journal.observed_lifecycle = true;
        (generation, snapshot.input_closed())
    } else {
        let snapshot = <LoggerTaskHost as TaskHost<MainEffects>>::snapshot(context.host());
        (snapshot.generation(), snapshot.extra().input_closed())
    };
    context.observe_host_generation(generation);
    Ok(RequestResult::Return(Value::atom_from_text(
        if input_closed { "closed" } else { "open" },
    )))
}

fn read_log(
    context: &mut RequestContext<'_, MainEffects>,
) -> Result<RequestResult, glam::reflection::TaskHalt> {
    let diagnostic_reader = context.host().input.diagnostic_reader.clone();
    if let Some(generation) = context.transaction_generation() {
        context.observe_host_generation(generation);
        let mut transaction = context
            .transaction()
            .expect("checked active reflection transaction");
        let (snapshot, journal) = transaction.parts();
        if let Some(value) = event_journal(snapshot, journal)
            .read(&diagnostic_reader)
            .map_err(glam::reflection::TaskHalt::from)?
        {
            return Diagnostic::from_transport_value(&value)
                .and_then(|diagnostic| diagnostic.enrich(&context.host().input.runtime.values()))
                .map(RequestResult::Return)
                .map_err(glam::reflection::TaskHalt::from);
        }
        // Queue reads observe only the host snapshot. Journaled writes remain
        // invisible until commit, just as writes from concurrent tasks do.
        return Ok(RequestResult::Fail);
    }

    loop {
        let snapshot = <LoggerTaskHost as TaskHost<MainEffects>>::snapshot(context.host());
        context.observe_host_generation(snapshot.generation());
        let mut events = RuntimeEventJournal::new(snapshot.extra().events().clone());
        let Some(value) = events
            .read(&diagnostic_reader)
            .map_err(glam::reflection::TaskHalt::from)?
        else {
            return Ok(RequestResult::Fail);
        };
        let value = Diagnostic::from_transport_value(&value)
            .and_then(|diagnostic| diagnostic.enrich(&context.host().input.runtime.values()))
            .map_err(glam::reflection::TaskHalt::from)?;
        let commit = TaskCommit::new(
            glam::reflection::StoreJournal::new(snapshot.store().clone()),
            snapshot.extra().clone(),
            MainJournal {
                reflection: ReflectionJournal::default(),
                events: Some(events),
                observed_lifecycle: false,
            },
        );
        match <LoggerTaskHost as TaskHost<MainEffects>>::commit(context.host(), commit) {
            CommitResult::Committed => {
                context.committed();
                return Ok(RequestResult::Return(value));
            }
            CommitResult::Conflict => {}
            CommitResult::MissingVolume(volume) => {
                return Err(glam::reflection::TaskHalt::new(format!(
                    "reflection volume {} was revoked before its edits committed",
                    volume.get()
                )));
            }
            CommitResult::Closed => return Ok(RequestResult::Cancelled),
        }
    }
}

struct LogHost {
    runtime: EvaluationRuntime,
    _diagnostic_ingress: DiagnosticIngress,
    diagnostic_reader: RuntimeInputReader,
}

/// Capabilities and mutable state belonging to the logger's evaluation
/// session. Incoming assembler diagnostics remain in `input`; diagnostics
/// emitted by this session go only to its diagnostic bus.
struct LoggerTaskHost {
    input: Arc<LogHost>,
    diagnostics: DiagnosticBus,
    reflection_environment: Value,
    diagnostic_writer: RuntimeOutputWriter,
    diagnostic_delivery: RuntimeOutputDelivery<Diagnostic>,
    stderr_writer: RuntimeOutputWriter,
    stderr_delivery: RuntimeOutputDelivery<Bytes>,
}

impl LoggerTaskHost {
    fn new(input: Arc<LogHost>, diagnostics: DiagnosticBus, reflection_environment: Value) -> Self {
        diagnostics
            .bind_runtime(&input.runtime)
            .expect("logger output bus must belong to the logger runtime");
        let diagnostic_bus = diagnostics.clone();
        let runtime_id = input.runtime.id();
        let diagnostic_output = input
            .runtime
            .output_endpoint(
                |value| Diagnostic::from_transport_value(&value),
                move |diagnostic| {
                    diagnostic_bus.publish_from_runtime(runtime_id, diagnostic)?;
                    Ok(())
                },
            )
            .expect("logger diagnostic output endpoint should be constructible");
        let stderr_output = input
            .runtime
            .output_endpoint(
                |value| {
                    value
                        .as_binary()
                        .map(Bytes::copy_from_slice)
                        .ok_or_else(|| Error::new("stderr output requires binary data"))
                },
                |bytes: Bytes| {
                    io::stderr()
                        .write_all(&bytes)
                        .map_err(|error| Error::new(error.to_string()))
                },
            )
            .expect("logger stderr output endpoint should be constructible");
        let (diagnostic_writer, diagnostic_delivery) = diagnostic_output.into_parts();
        let (stderr_writer, stderr_delivery) = stderr_output.into_parts();
        Self {
            input,
            diagnostics,
            reflection_environment,
            diagnostic_writer,
            diagnostic_delivery,
            stderr_writer,
            stderr_delivery,
        }
    }

    fn write_diagnostic(&self, diagnostic: Diagnostic) -> Result<(), Error> {
        let (_generation, store, snapshot) = self.input.runtime.logger_transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot.events().clone());
        events.write(
            &self.diagnostic_writer,
            diagnostic.transport_value(&self.input.runtime.values()),
        )?;
        match self.input.runtime.try_commit_logger_transaction(
            &glam::reflection::StoreJournal::new(store),
            &snapshot,
            false,
            &events,
        ) {
            glam::reflection::StoreCommitResult::Committed => self.deliver_outputs(),
            glam::reflection::StoreCommitResult::Conflict => Err(Error::new(
                "logger output conflicted during immediate commit",
            )),
            glam::reflection::StoreCommitResult::MissingVolume(volume) => Err(Error::new(format!(
                "reflection volume {} was revoked before logger output committed",
                volume.get()
            ))),
        }
    }

    fn write_stderr(&self, bytes: Bytes) -> Result<(), Error> {
        let (_generation, store, snapshot) = self.input.runtime.logger_transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot.events().clone());
        events.write(&self.stderr_writer, Value::binary(bytes))?;
        match self.input.runtime.try_commit_logger_transaction(
            &glam::reflection::StoreJournal::new(store),
            &snapshot,
            false,
            &events,
        ) {
            glam::reflection::StoreCommitResult::Committed => self.deliver_outputs(),
            glam::reflection::StoreCommitResult::Conflict => Err(Error::new(
                "stderr output conflicted during immediate commit",
            )),
            glam::reflection::StoreCommitResult::MissingVolume(volume) => Err(Error::new(format!(
                "reflection volume {} was revoked before stderr output committed",
                volume.get()
            ))),
        }
    }

    fn deliver_outputs(&self) -> Result<(), Error> {
        loop {
            let mut delivered = false;
            if let Some(outcome) = self.diagnostic_delivery.deliver_next()? {
                delivered = true;
                if let RuntimeDeliveryOutcome::Failed(failure) = outcome {
                    return Err(failure.error().clone());
                }
            }
            if let Some(outcome) = self.stderr_delivery.deliver_next()? {
                delivered = true;
                if let RuntimeDeliveryOutcome::Failed(failure) = outcome {
                    return Err(failure.error().clone());
                }
            }
            if !delivered {
                return Ok(());
            }
        }
    }
}

impl LogHost {
    #[cfg(test)]
    fn new(diagnostics: &DiagnosticBus) -> Self {
        let runtime =
            EvaluationRuntime::new(0).expect("test logger runtime should be constructible");
        Self::with_runtime(runtime, diagnostics)
    }

    fn with_runtime(runtime: EvaluationRuntime, diagnostics: &DiagnosticBus) -> Self {
        let (ingress, diagnostic_reader) = diagnostics
            .diagnostic_ingress(&runtime)
            .expect("logger diagnostic ingress should be constructible");
        Self {
            runtime,
            _diagnostic_ingress: ingress,
            diagnostic_reader,
        }
    }

    fn close_input(&self) {
        self.runtime.close_logger_input();
    }

    fn cancel(&self) {
        self.runtime.cancel_logger();
    }

    fn drain_default(&self, logger: &DefaultLogger) {
        while let Some(diagnostic) = self.take_diagnostic() {
            logger.emit(&diagnostic);
        }
    }

    fn take_diagnostic(&self) -> Option<Diagnostic> {
        loop {
            let (generation, store, snapshot) = self.runtime.logger_transaction_snapshot();
            let mut events = RuntimeEventJournal::new(snapshot.events().clone());
            let value = events
                .read(&self.diagnostic_reader)
                .expect("logger diagnostic endpoint should match its runtime");
            if let Some(value) = value {
                match self.runtime.try_commit_logger_transaction(
                    &glam::reflection::StoreJournal::new(store),
                    &snapshot,
                    false,
                    &events,
                ) {
                    glam::reflection::StoreCommitResult::Committed => {
                        return Some(
                            Diagnostic::from_transport_value(&value)
                                .expect("diagnostic ingress stores transport envelopes"),
                        );
                    }
                    glam::reflection::StoreCommitResult::Conflict => continue,
                    glam::reflection::StoreCommitResult::MissingVolume(volume) => {
                        panic!(
                            "unchanged logger reflection snapshot lost volume {}",
                            volume.get()
                        );
                    }
                }
            }
            if snapshot.cancelled() || snapshot.input_closed() {
                return None;
            }
            self.runtime.wait_for_change(generation);
        }
    }
}

impl TaskEnvironment for LoggerTaskHost {
    fn reflection_environment(&self) -> Value {
        self.reflection_environment.clone()
    }
}

impl ReflectionServices for LoggerTaskHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        if let Err(error) = self.write_diagnostic(diagnostic) {
            self.diagnostics.publish(error.diagnostic().clone());
        }
    }

    fn update_query(&self, handle: &Arc<glam::reflection::EvaluationQueryHandle>, result: Value) {
        self.input.runtime.update_query(handle, result);
    }
}

impl TaskHost<MainEffects> for LoggerTaskHost {
    fn snapshot(&self) -> HostSnapshot<MainEffects> {
        let (generation, store, input) = self.input.runtime.logger_transaction_snapshot();
        HostSnapshot::new(generation, store, input)
    }

    fn commit(&self, commit: TaskCommit<MainEffects>) -> CommitResult {
        let (store, snapshot, journal) = commit.into_parts();
        let mut events = journal
            .events
            .unwrap_or_else(|| RuntimeEventJournal::new(snapshot.events().clone()));
        for diagnostic in journal.reflection.diagnostics() {
            if let Err(error) = events.write(
                &self.diagnostic_writer,
                diagnostic.transport_value(&self.input.runtime.values()),
            ) {
                self.diagnostics.publish(error.diagnostic().clone());
                return CommitResult::Closed;
            }
        }
        match self.input.runtime.try_commit_logger_transaction(
            &store,
            &snapshot,
            journal.observed_lifecycle,
            &events,
        ) {
            glam::reflection::StoreCommitResult::Committed => {}
            glam::reflection::StoreCommitResult::Conflict => {
                return CommitResult::Conflict;
            }
            glam::reflection::StoreCommitResult::MissingVolume(volume) => {
                return CommitResult::MissingVolume(volume);
            }
        }
        journal.reflection.commit_updates();
        if let Err(error) = self.deliver_outputs() {
            self.diagnostics.publish(error.diagnostic().clone());
        }
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        let (generation, _, snapshot) = self.input.runtime.logger_transaction_snapshot();
        if snapshot.cancelled() {
            return false;
        }
        if snapshot.input_closed() {
            return generation != observed_generation;
        }
        self.input.runtime.wait_for_change(observed_generation);
        let (generation, _, snapshot) = self.input.runtime.logger_transaction_snapshot();
        !snapshot.cancelled() && (!snapshot.input_closed() || generation != observed_generation)
    }

    fn evaluation_runtime_id(&self) -> Option<glam::EvaluationRuntimeId> {
        Some(self.input.runtime.id())
    }
}

fn empty_environment_object(values: &glam::Values) -> Value {
    values.empty_object(values.abstract_global_path(["configuration", "env"]))
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
    const AUTO_INDENT: usize = 4;
    const ANCHOR_INDENT: usize = 2;

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
        let rendered = self
            .format_diagnostic(diagnostic, &terminal)
            .unwrap_or_else(|_| {
                Bytes::from(self.render(diagnostic, diagnostic.message(), &terminal))
            });

        let _ = io::stderr().lock().write_all(&rendered);
    }

    fn format_diagnostic(
        &self,
        diagnostic: &Diagnostic,
        terminal: &TerminalContext,
    ) -> Result<Bytes, Error> {
        let values = self.evaluator.values();
        let message = diagnostic.enrich_with(&values, self.viewer_updates(diagnostic, terminal))?;
        let context_lines = self.context_lines(&message, terminal, 0);
        let message = Diagnostic::apply_updates(
            &values,
            &message,
            Self::context_lines_update(context_lines),
        )?;
        self.format_message(message)
    }

    fn format_message(&self, message: Value) -> Result<Bytes, Error> {
        self.evaluator
            .apply(&self.formatter, [message])
            .and_then(|rendered| self.evaluator.to_binary(&rendered))
    }

    fn viewer_updates(&self, diagnostic: &Diagnostic, terminal: &TerminalContext) -> Value {
        let header = format!(
            "{}{}",
            self.location(diagnostic),
            Self::severity_header(diagnostic.severity(), terminal)
        );
        let source = diagnostic.source().and_then(|source| {
            let path = Path::new(source);
            path.is_absolute().then(|| self.display_source(path))
        });
        self.terminal_viewer_updates(terminal, 0, header, self.location(diagnostic), source)
    }

    fn terminal_viewer_updates(
        &self,
        terminal: &TerminalContext,
        base_indent: usize,
        header: String,
        location: String,
        source: Option<String>,
    ) -> Value {
        let mut viewer = vec![
            ("kind", Value::text("terminal")),
            (
                "columns",
                Value::integer(i64::try_from(terminal.columns).unwrap_or(i64::MAX)),
            ),
            ("color", Value::text(terminal.color.name())),
            ("header", Value::text(header)),
            ("auto_indent", Value::integer(Self::AUTO_INDENT as i64)),
            (
                "indent",
                Value::text(" ".repeat(base_indent + Self::AUTO_INDENT)),
            ),
            (
                "anchor_indent",
                Value::text(" ".repeat(base_indent + Self::ANCHOR_INDENT)),
            ),
            ("location", Value::text(location)),
            ("context_lines", Value::list(std::iter::empty())),
        ];
        if let Some(term) = &terminal.term {
            viewer.push(("term", Value::text(term)));
        }
        if let Some(language) = &terminal.language {
            viewer.push(("lang", Value::text(language)));
        }
        if let Some(source) = source {
            viewer.push(("source", Value::record([("file", Value::text(source))])));
        }
        Value::record([("viewer", Value::record(viewer))])
    }

    fn context_lines(
        &self,
        message: &Value,
        terminal: &TerminalContext,
        base_indent: usize,
    ) -> Vec<String> {
        let contexts = match self.evaluator.get_optional(message, "msg.context") {
            Ok(Some(contexts)) => contexts,
            Ok(None) => return Vec::new(),
            Err(error) => {
                return vec![
                    format!("{}context:", " ".repeat(base_indent + Self::ANCHOR_INDENT)),
                    format!(
                        "{}msg: <context rendering failed: {error}>",
                        " ".repeat(base_indent + Self::AUTO_INDENT)
                    ),
                ];
            }
        };
        let reflection = self.evaluator.reflection();
        let contexts = reflection.evaluate(&contexts).unwrap_or(contexts);
        let frames = if contexts.is_undefined() {
            Vec::new()
        } else if contexts.kind() == glam::ValueKind::List {
            reflection
                .list_items(&contexts)
                .unwrap_or_else(|_| vec![contexts])
        } else {
            vec![contexts]
        };
        if frames.is_empty() {
            return Vec::new();
        }

        let mut lines = Vec::with_capacity(frames.len() + 1);
        lines.push(format!(
            "{}context:",
            " ".repeat(base_indent + Self::ANCHOR_INDENT)
        ));
        lines.extend(frames.into_iter().map(|frame| {
            self.render_context_frame(&frame, terminal, base_indent + Self::AUTO_INDENT)
        }));
        lines
    }

    fn render_context_frame(
        &self,
        frame: &Value,
        terminal: &TerminalContext,
        frame_indent: usize,
    ) -> String {
        let frame = match self.evaluator.reflection().evaluate(frame) {
            Ok(frame) => frame,
            Err(error) => {
                return format!(
                    "{}msg: <context rendering failed: {error}>",
                    " ".repeat(frame_indent)
                );
            }
        };
        let message_tag = Value::atom_from_text("msg");
        let is_message = self
            .evaluator
            .reflection()
            .dictionary_items(&frame)
            .is_ok_and(|items| items.into_iter().any(|(tag, _)| tag == message_tag));
        if is_message {
            return self
                .render_context_message(&frame, terminal, frame_indent)
                .unwrap_or_else(|error| {
                    format!(
                        "{}msg: <context rendering failed: {error}>",
                        " ".repeat(frame_indent)
                    )
                });
        }
        format!(
            "{}{}",
            " ".repeat(frame_indent),
            self.summarize_context_frame(&frame)
        )
    }

    fn render_context_message(
        &self,
        message: &Value,
        terminal: &TerminalContext,
        frame_indent: usize,
    ) -> Result<String, Error> {
        let default_header = "msg: ".to_owned();
        let values = self.evaluator.values();
        let message = Diagnostic::apply_updates(
            &values,
            message,
            self.terminal_viewer_updates(
                terminal,
                frame_indent,
                default_header.clone(),
                String::new(),
                None,
            ),
        )?;
        let header = self.context_message_header(&message, terminal);
        let message = if header == default_header {
            message
        } else {
            Diagnostic::apply_updates(&values, &message, Self::viewer_header_update(header))?
        };
        let context_lines = self.context_lines(&message, terminal, frame_indent);
        let message = Diagnostic::apply_updates(
            &values,
            &message,
            Self::context_lines_update(context_lines),
        )?;
        let rendered = self.format_message(message)?;
        let rendered = String::from_utf8_lossy(&rendered);
        let rendered = rendered.strip_suffix('\n').unwrap_or(&rendered);
        Ok(format!("{}{rendered}", " ".repeat(frame_indent)))
    }

    fn context_message_header(&self, message: &Value, terminal: &TerminalContext) -> String {
        let Some(severity) = self
            .evaluator
            .get_optional(message, "msg.severity")
            .ok()
            .flatten()
        else {
            return "msg: ".to_owned();
        };
        let Ok(key) = self.evaluator.reflection().atom_key(&severity) else {
            return "msg: ".to_owned();
        };
        match immediate_diagnostic_text(&key).as_deref() {
            Some("info") => Self::severity_header(Severity::Info, terminal),
            Some("warn") => Self::severity_header(Severity::Warning, terminal),
            Some("error") => Self::severity_header(Severity::Error, terminal),
            _ => "msg: ".to_owned(),
        }
    }

    fn viewer_header_update(header: String) -> Value {
        Value::record([("viewer", Value::record([("header", Value::text(header))]))])
    }

    fn context_lines_update(lines: Vec<String>) -> Value {
        Value::record([(
            "viewer",
            Value::record([(
                "context_lines",
                Value::list(lines.into_iter().map(Value::text)),
            )]),
        )])
    }

    fn summarize_context_frame(&self, frame: &Value) -> String {
        let reflection = self.evaluator.reflection();
        let Ok(entries) = reflection.dictionary_items(frame) else {
            return diagnostic_value_kind(&self.evaluator.values(), frame).to_owned();
        };
        let [(tag, payload)] = entries.as_slice() else {
            return diagnostic_value_kind(&self.evaluator.values(), frame).to_owned();
        };

        if tag == &Value::atom_from_text("eval") {
            return self.eval_context_summary(payload);
        }
        if tag == &Value::atom_from_text("g") {
            return self.g_context_summary(payload);
        }
        if tag == &Value::atom_from_text("import") {
            return self.import_context_summary(payload);
        }
        if tag == &Value::atom_from_text("asm") {
            return self.asm_context_summary(payload);
        }
        if tag == &Value::atom_from_text("conf") {
            return self.conf_context_summary(payload);
        }
        if tag == &Value::atom_from_text("task") {
            return self.task_context_summary(payload);
        }
        self.context_tag_text(tag)
            .unwrap_or_else(|| diagnostic_value_kind(&self.evaluator.values(), frame).to_owned())
    }

    fn eval_context_summary(&self, payload: &Value) -> String {
        let operation = self
            .context_field_tag_text(payload, "op")
            .map(|operation| operation.replace('_', " "));
        let path = self.context_field_text(payload, "args.path");
        match (operation, path) {
            (Some(operation), Some(path)) => format!("eval: {operation} `{path}`"),
            (Some(operation), None) => format!("eval: {operation}"),
            (None, Some(path)) => format!("eval: path `{path}`"),
            (None, None) => "eval".to_owned(),
        }
    }

    fn g_context_summary(&self, payload: &Value) -> String {
        let definition = self.context_field_text(payload, "definition");
        let line = self.context_field_text(payload, "line");
        match (definition, line) {
            (Some(definition), Some(line)) => {
                format!("g: definition `{definition}` on line {line}")
            }
            (Some(definition), None) => format!("g: definition `{definition}`"),
            (None, Some(line)) => format!("g: line {line}"),
            (None, None) => "g".to_owned(),
        }
    }

    fn import_context_summary(&self, payload: &Value) -> String {
        self.context_field_text(payload, "request.file")
            .map_or_else(
                || "import".to_owned(),
                |request| format!("import: request `{request}`"),
            )
    }

    fn asm_context_summary(&self, payload: &Value) -> String {
        self.context_field_text(payload, "result").map_or_else(
            || "asm".to_owned(),
            |result| format!("asm: result `{result}`"),
        )
    }

    fn conf_context_summary(&self, payload: &Value) -> String {
        self.context_field_text(payload, "entry").map_or_else(
            || "conf".to_owned(),
            |entry| format!("conf: entry `{entry}`"),
        )
    }

    fn task_context_summary(&self, payload: &Value) -> String {
        let operation = self.context_field_tag_text(payload, "operation");
        let id = self.context_field_text(payload, "id");
        match (operation, id) {
            (Some(operation), Some(id)) => format!("task: {operation} task {id}"),
            (Some(operation), None) => format!("task: {operation}"),
            (None, Some(id)) => format!("task: task {id}"),
            (None, None) => "task".to_owned(),
        }
    }

    fn context_field_text(&self, value: &Value, path: &str) -> Option<String> {
        self.evaluator
            .get(value, path)
            .ok()
            .and_then(|value| immediate_diagnostic_text(&value))
    }

    fn context_field_tag_text(&self, value: &Value, path: &str) -> Option<String> {
        self.evaluator
            .get(value, path)
            .ok()
            .and_then(|value| self.context_tag_text(&value))
    }

    fn context_tag_text(&self, tag: &Value) -> Option<String> {
        immediate_diagnostic_text(tag).or_else(|| {
            self.evaluator
                .reflection()
                .atom_key(tag)
                .ok()
                .and_then(|key| immediate_diagnostic_text(&key))
        })
    }

    fn severity_header(severity: Severity, terminal: &TerminalContext) -> String {
        let label = severity.to_string();
        format!("{}: ", terminal.color.paint(severity, &label))
    }

    fn render(&self, diagnostic: &Diagnostic, text: &str, terminal: &TerminalContext) -> String {
        let severity = diagnostic.severity().to_string();
        let severity = terminal.color.paint(diagnostic.severity(), &severity);
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
        for line in self.context_lines(diagnostic.emission(), terminal, 0) {
            rendered.push('\n');
            rendered.push_str(&line);
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

fn immediate_diagnostic_text(value: &Value) -> Option<String> {
    value
        .as_binary()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .or_else(|| value.as_number_text())
}

fn diagnostic_value_kind(values: &glam::Values, value: &Value) -> &'static str {
    if value.is_undefined() {
        return "Undefined";
    }
    if value == &values.abstract_global_path(["builtin", "unit"]) {
        return "Unit";
    }
    match value.kind() {
        glam::ValueKind::Atom => "Atom",
        glam::ValueKind::Number => "Number",
        glam::ValueKind::Binary => "Binary",
        glam::ValueKind::List => "List",
        glam::ValueKind::Dict => "Dict",
        glam::ValueKind::Function => "Function",
        glam::ValueKind::Net => "Net",
        glam::ValueKind::Lazy => "Lazy",
        glam::ValueKind::Sealed => "Sealed",
        glam::ValueKind::Opaque => "Opaque",
        _ => "Value",
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

fn inspect_g_source_command(path: &Path, verbosity: glam::cli::ParseVerbosity) -> ExitCode {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("error: could not read `{}`: {error}", path.display());
            return ExitCode::from(1);
        }
    };
    let parsed = inspect_g_source(&bytes);
    let output = format_parse_summary(path, &parsed, verbosity);
    if io::stdout().write_all(output.as_bytes()).is_err() {
        eprintln!("error: could not write parse inspection to stdout");
        return ExitCode::from(1);
    }

    if parsed.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
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
        let queue = Arc::new(LogHost::new(&diagnostics));

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
        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("the closed Glam formatter should return bytes");

        assert_eq!(
            rendered,
            Bytes::from_static(b"src/test.g:4: warning: first\n    second\n    \n    fourth\n")
        );
    }

    #[test]
    fn glam_default_formatter_renders_recognized_context_frames() {
        let evaluator = Assembler::default();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Error,
            Value::record([(
                "msg",
                Value::record([
                    ("text", Value::text("broken\nmore detail")),
                    (
                        "context",
                        Value::list([
                            Value::record([(
                                "eval",
                                Value::record([("op", Value::atom_from_text("binary_extraction"))]),
                            )]),
                            Value::record([(
                                "g",
                                Value::record([
                                    ("definition", Value::text("result")),
                                    ("line", Value::integer(7)),
                                ]),
                            )]),
                            Value::record([(
                                "import",
                                Value::record([(
                                    "request",
                                    Value::record([("file", Value::text("child.g"))]),
                                )]),
                            )]),
                            Value::record([(
                                "asm",
                                Value::record([("result", Value::text("asm.result"))]),
                            )]),
                            Value::record([(
                                "eval",
                                Value::record([
                                    ("op", Value::atom_from_text("path_lookup")),
                                    ("args", Value::record([("path", Value::text("conf.env"))])),
                                ]),
                            )]),
                            Value::record([(
                                "conf",
                                Value::record([("entry", Value::text("log"))]),
                            )]),
                            Value::record([(
                                "task",
                                Value::record([
                                    ("operation", Value::atom_from_text("join")),
                                    ("id", Value::integer(12)),
                                ]),
                            )]),
                        ]),
                    ),
                ]),
            )]),
        );
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: None,
            language: None,
        };
        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("the closed Glam formatter should render contexts");

        assert_eq!(
            rendered,
            Bytes::from_static(
                b"error: broken\n    more detail\n  context:\n    eval: binary extraction\n    g: definition `result` on line 7\n    import: request `child.g`\n    asm: result `asm.result`\n    eval: path lookup `conf.env`\n    conf: entry `log`\n    task: join task 12\n"
            )
        );
    }

    #[test]
    fn glam_default_formatter_recursively_renders_context_messages() {
        let evaluator = Assembler::default();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Error,
            Value::record([(
                "msg",
                Value::record([
                    ("text", Value::text("outer failure")),
                    (
                        "context",
                        Value::list([
                            Value::record([(
                                "msg",
                                Value::record([("text", Value::text("unclassified context"))]),
                            )]),
                            Value::record([(
                                "msg",
                                Value::record([
                                    ("text", Value::text("nested context\nmore detail")),
                                    ("severity", Value::atom_from_text("info")),
                                    (
                                        "context",
                                        Value::list([Value::record([(
                                            "eval",
                                            Value::record([(
                                                "op",
                                                Value::atom_from_text("list_index"),
                                            )]),
                                        )])]),
                                    ),
                                ]),
                            )]),
                        ]),
                    ),
                ]),
            )]),
        );
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: Some("xterm-256color".to_owned()),
            language: Some("en_US.UTF-8".to_owned()),
        };

        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("context messages should use the recursive diagnostic view");

        assert_eq!(
            rendered,
            Bytes::from_static(
                b"error: outer failure\n  context:\n    msg: unclassified context\n    info: nested context\n        more detail\n      context:\n        eval: list index\n"
            )
        );
    }

    #[test]
    fn glam_default_formatter_recognizes_full_objects_as_context_messages() {
        let evaluator = Assembler::default();
        let module = evaluator
            .module(["context_fixture"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "object frame with\n",
                    "  msg = {text:viewer.term, severity:'info}\n",
                ),
            )
            .build()
            .expect("context object fixture should compile");
        let frame = evaluator
            .get(module.value(), "frame")
            .expect("context object should be available");
        assert!(
            evaluator.get(&frame, "spec").is_ok(),
            "fixture must retain its object interface"
        );
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Error,
            Value::record([(
                "msg",
                Value::record([
                    ("text", Value::text("outer failure")),
                    ("context", Value::list([frame])),
                ]),
            )]),
        );
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: Some("object terminal context".to_owned()),
            language: None,
        };

        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("a context object should retain its view behavior");

        assert_eq!(
            rendered,
            Bytes::from_static(
                b"error: outer failure\n  context:\n    info: object terminal context\n"
            )
        );
    }

    #[test]
    fn failed_context_message_rendering_does_not_hide_the_primary_diagnostic() {
        let evaluator = Assembler::default();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Error,
            Value::record([(
                "msg",
                Value::record([
                    ("text", Value::text("outer failure")),
                    (
                        "context",
                        Value::list([Value::record([(
                            "msg",
                            Value::record([("text", Value::integer(42))]),
                        )])]),
                    ),
                ]),
            )]),
        );
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: None,
            language: None,
        };

        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("a malformed context message should have a local fallback");
        let rendered = String::from_utf8_lossy(&rendered);

        assert!(rendered.starts_with("error: outer failure\n  context:\n"));
        assert!(rendered.contains("    msg: <context rendering failed:"));
    }

    #[test]
    fn glam_default_formatter_summarizes_unrecognized_context_frames() {
        let evaluator = Assembler::default();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Warning,
            Value::record([(
                "msg",
                Value::record([
                    ("text", Value::text("careful")),
                    (
                        "context",
                        Value::list([
                            Value::record([("custom", Value::integer(42))]),
                            Value::record([
                                ("left", Value::integer(1)),
                                ("right", Value::integer(2)),
                            ]),
                            Value::integer(7),
                        ]),
                    ),
                ]),
            )]),
        );
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: None,
            language: None,
        };
        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("the closed Glam formatter should summarize unknown contexts");

        assert_eq!(
            rendered,
            Bytes::from_static(b"warning: careful\n  context:\n    custom\n    Dict\n    Number\n")
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
        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
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
        let values = logger.evaluator.values();
        let enriched = diagnostic
            .enrich_with(&values, logger.viewer_updates(&diagnostic, &terminal))
            .expect("terminal viewer metadata should mix into a diagnostic");

        assert_eq!(
            logger
                .evaluator
                .get(&enriched, "viewer.auto_indent")
                .expect("viewer should declare automatic indentation")
                .as_i64(),
            Some(4)
        );
        assert_eq!(
            logger
                .evaluator
                .get(&enriched, "viewer.header")
                .expect("viewer should materialize the complete message header")
                .as_binary(),
            Some(b"\x1b[36minfo\x1b[0m: ".as_slice())
        );
        assert_eq!(
            logger
                .evaluator
                .get(&enriched, "viewer.anchor_indent")
                .expect("viewer should expose its section anchor indentation")
                .as_binary(),
            Some(b"  ".as_slice())
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
    fn assembly_result_context_names_the_executable_output_boundary() {
        let assembler = Assembler::default();
        assert_eq!(
            assembler
                .get(&assembly_result_context(), "asm.result")
                .expect("assembly result context should identify its output")
                .as_binary(),
            Some(b"asm.result".as_slice())
        );
    }

    #[test]
    fn bus_error_count_survives_absent_subscribers_and_queue_reads() {
        let diagnostics = DiagnosticBus::new();
        diagnostics.publish(Diagnostic::new(Severity::Error, "dropped"));
        assert_eq!(diagnostics.counts().errors(), 1);

        let retained = Arc::new(LogHost::new(&diagnostics));
        diagnostics.publish(Diagnostic::new(Severity::Error, "retained"));
        assert!(retained.take_diagnostic().is_some());
        assert_eq!(diagnostics.counts().errors(), 2);
        retained.close_input();
    }

    #[test]
    fn logger_wait_retries_an_unseen_stream_closure_once() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("logger test assembler should build");
        let host = LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::new(),
            assembler.reflection_environment_for_role("logger"),
        );
        let (open_generation, _, _) = input.runtime.logger_transaction_snapshot();

        input.close_input();

        assert!(<LoggerTaskHost as TaskHost<MainEffects>>::wait_for_change(
            &host,
            open_generation
        ));
        let (closed_generation, _, snapshot) = input.runtime.logger_transaction_snapshot();
        assert!(snapshot.input_closed());
        assert!(!<LoggerTaskHost as TaskHost<MainEffects>>::wait_for_change(
            &host,
            closed_generation
        ));
    }

    #[test]
    fn logger_session_output_is_separate_from_assembler_input() {
        struct Capture(Arc<std::sync::Mutex<Vec<DiagnosticEvent>>>);
        impl DiagnosticSubscriber for Capture {
            fn receive(&self, event: DiagnosticEvent) {
                self.0
                    .lock()
                    .expect("output capture should not be poisoned")
                    .push(event);
            }
        }

        let input_diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&input_diagnostics));
        let diagnostics = DiagnosticBus::new();
        let output = Arc::new(std::sync::Mutex::new(Vec::new()));
        let _subscription = diagnostics.subscribe(Capture(output.clone()));
        let host = LoggerTaskHost::new(
            input.clone(),
            diagnostics.clone(),
            Assembler::default().reflection_environment_for_role("logger"),
        );

        <LoggerTaskHost as ReflectionServices>::emit_diagnostic(
            &host,
            Diagnostic::new(Severity::Error, "session output"),
        );

        let (_generation, _store, snapshot) = input.runtime.logger_transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot.events().clone());
        assert_eq!(
            events.read(&input.diagnostic_reader).unwrap(),
            None,
            "logger output must not return to assembler diagnostic input"
        );
        let output = output
            .lock()
            .expect("output capture should not be poisoned")
            .first()
            .cloned()
            .expect("logger output bus should publish the diagnostic");
        assert_eq!(output.message(), "session output");
        assert_eq!(diagnostics.counts().errors(), 1);
    }
}
