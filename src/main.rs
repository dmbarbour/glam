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
    CommitResult, EffectLifecycle, EffectRequestSpec, EffectRun, HostSnapshot, ReflectionJournal,
    ReflectionQueryWriter, ReflectionRequest, ReflectionServices, ReflectionTransaction,
    RequestContext, RequestResult, TaskCommit, TaskEnvironment, TaskHost, TaskOutcome,
    TaskSpecialization, handle_reflection_request, reflection_request_specs,
};
use glam::{
    Assembler, Diagnostic, DiagnosticBus, DiagnosticEvent, DiagnosticIngress, DiagnosticSubscriber,
    Error, EvaluationRuntime, FileSourceSystem, ModuleInput, PromiseResolver, QuiescenceReport,
    RuntimeDeadlockWork, RuntimeDeliveryFailure, RuntimeDeliveryOutcome, RuntimeDependency,
    RuntimeDisposition, RuntimeDispositionKind, RuntimeEventJournal, RuntimeEventSnapshot,
    RuntimeInputReader, RuntimeKillReason, RuntimeOutputDelivery, RuntimeOutputWriter,
    RuntimeReadiness, RuntimeTaskCapability, RuntimeWorkKind, RuntimeWorkState, Severity, Value,
    Values, check_local_manifest, inspect_g_source,
};

trait DiagnosticBusLocal {
    fn publish_local(&self, diagnostic: Diagnostic) -> DiagnosticEvent;
}

impl DiagnosticBusLocal for DiagnosticBus {
    fn publish_local(&self, diagnostic: Diagnostic) -> DiagnosticEvent {
        self.publish(diagnostic)
            .expect("diagnostic and bus must belong to the same evaluation runtime")
    }
}

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
                        .publish_local(diagnostic.clone());
                }
                prepared.assembler.diagnostic_bus().publish_local(
                    error
                        .diagnostic(&prepared.assembler.values())
                        .expect("configured CLI diagnostics belong to its assembler runtime"),
                );
                return finish_without_logger(prepared, None, true);
            }
        };
    for diagnostic in completion.diagnostics() {
        prepared
            .assembler
            .diagnostic_bus()
            .publish_local(diagnostic.clone());
    }
    let failed = write_completion(&completion).is_err_and(|error| {
        prepared
            .assembler
            .diagnostic_bus()
            .publish_local(Diagnostic::new(
                &prepared.assembler.values(),
                Severity::Error,
                error,
            ));
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
    let initial_environment = Some((Arc::from(cli_arguments.args().to_vec()), Arc::from([])));
    let prepared = match prepare_assembly(cli_arguments, None, initial_environment) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let values = prepared.assembler.values();
    let configured = values
        .access_names(
            &prepared.configuration.value,
            ["conf", "completion_script", name],
        )
        .and_then(|candidate| {
            with_path_lookup_context(
                &values,
                candidate,
                &format!("conf.completion_script.{name}"),
            )
        })
        .and_then(|candidate| {
            values.apply(&values.defined_or_function(), [values.list([])?, candidate])
        })
        .and_then(|selected| prepared.assembler.evaluator().eval(&selected))
        .ok()
        .and_then(|selected| {
            selected
                .array_items()
                .filter(Vec::is_empty)
                .map(|_| None)
                .unwrap_or_else(|| Some(selected.into_value()))
        });
    let output: Result<Vec<u8>, String> = match configured {
        Some(function) => configured_completion_script(&prepared.assembler, &function),
        None => builtin_completion_script(name)
            .map(|script| script.as_bytes().to_vec())
            .ok_or_else(|| format!("unknown completion script binding `{name}`")),
    };
    let failed = match output {
        Ok(output) => io::stdout().write_all(&output).is_err_and(|error| {
            prepared
                .assembler
                .diagnostic_bus()
                .publish_local(Diagnostic::new(
                    &prepared.assembler.values(),
                    Severity::Error,
                    format!("could not write completion script to stdout: {error}"),
                ));
            true
        }),
        Err(error) => {
            prepared
                .assembler
                .diagnostic_bus()
                .publish_local(Diagnostic::new(
                    &prepared.assembler.values(),
                    Severity::Error,
                    error,
                ));
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
    let values = assembler.values();
    let context = values
        .record([
            (
                "executable",
                values.bytes(executable.as_encoded_bytes().to_vec()),
            ),
            ("protocol", values.text("v0")),
            ("request", values.text("--completions")),
        ])
        .expect("completion-script context uses one runtime");
    values
        .apply(function, [context])
        .and_then(|value| values.anno_binary(value))
        .and_then(|binary| assembler.evaluator().eval(&binary))
        .and_then(|binary| {
            binary
                .as_bytes()
                .map(ToOwned::to_owned)
                .ok_or_else(|| Error::new("completion script did not evaluate to binary data"))
        })
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
    values: &Values,
    reflection_arguments: Value,
    process_arguments: Value,
    cli_arguments: CliArguments,
) -> Value {
    fn os_value(values: &Values, value: &std::ffi::OsStr) -> Value {
        values.bytes(value.as_encoded_bytes().to_vec())
    }

    let variables = values
        .dictionary(env::vars_os().map(|(name, value)| {
            (
                values.bytes(name.as_encoded_bytes().to_vec()),
                os_value(values, &value),
            )
        }))
        .expect("OS environment names must be keyable binary values");
    let cli_arguments = values
        .list(
            cli_arguments
                .args()
                .iter()
                .map(|argument| os_value(values, argument)),
        )
        .expect("CLI argument values share one runtime");
    values
        .record([(
            "process",
            values
                .record([
                    ("args", process_arguments),
                    ("env", variables),
                    ("refl_args", reflection_arguments),
                    (
                        "cli",
                        values
                            .record([("args", cli_arguments)])
                            .expect("CLI values share one runtime"),
                    ),
                ])
                .expect("process values share one runtime"),
        )])
        .expect("process environment values share one runtime")
}

fn argument_values(values: &Values, arguments: &[std::ffi::OsString]) -> Value {
    values
        .list(
            arguments
                .iter()
                .map(|argument| values.bytes(argument.as_encoded_bytes().to_vec())),
        )
        .expect("argument values share one runtime")
}

fn finish_local_files(
    files: &FileSourceSystem,
    manifest: Option<&Path>,
    diagnostics: &DiagnosticBus,
    values: &Values,
) -> bool {
    let mut failed = false;
    if let Err(warning) = files.verify_unchanged() {
        diagnostics.publish_local(Diagnostic::new(
            values,
            Severity::Warning,
            warning.to_string(),
        ));
    }
    if let Some(path) = manifest
        && let Err(error) = files.write_manifest(path)
    {
        failed = true;
        diagnostics.publish_local(Diagnostic::new(values, Severity::Error, error.to_string()));
    }
    failed
}

fn publish_error(diagnostics: &DiagnosticBus, values: &Values, error: &Error) {
    diagnostics.publish_local(
        error
            .diagnostic(values)
            .expect("errors published by main belong to its active runtime"),
    );
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
            resolver.resolve(argument_values(
                &self.assembler.values(),
                &command.process_args,
            ))?;
        }
        if let Some(resolver) = self.reflection_args.take() {
            resolver.resolve(argument_values(
                &self.assembler.values(),
                &command.reflection_args,
            ))?;
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

type InitialCliEnvironment = (Arc<[std::ffi::OsString]>, Arc<[std::ffi::OsString]>);

fn prepare_assembly(
    cli_arguments: CliArguments,
    failure_manifest: Option<&Path>,
    initial_environment: Option<InitialCliEnvironment>,
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
                &environment.values(),
                reflection_value,
                process_value,
                cli_arguments,
            ))
        })
        .expect("main's reflection environment must be a dictionary")
        .build()
        .expect("main's assembler configuration must be valid");
    if let Some((process_arguments, reflection_arguments)) = initial_environment {
        let values = assembler.values();
        process_args
            .take()
            .expect("bootstrap process argument resolver should be present")
            .resolve(argument_values(&values, &process_arguments))
            .expect("fresh bootstrap process argument promise should resolve");
        reflection_args
            .take()
            .expect("bootstrap reflection argument resolver should be present")
            .resolve(argument_values(&values, &reflection_arguments))
            .expect("fresh bootstrap reflection argument promise should resolve");
    }
    let configuration = match load_configuration(&assembler) {
        Ok(configuration) => configuration,
        Err(error) => {
            let diagnostics = assembler.diagnostic_bus();
            publish_error(&diagnostics, &assembler.values(), &error);
            finish_local_files(
                &local_files,
                failure_manifest,
                &diagnostics,
                &assembler.values(),
            );
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
        Arc::from(command.process_args().to_vec()),
        Arc::from(command.reflection_args().to_vec()),
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
        publish_error(
            &prepared.assembler.diagnostic_bus(),
            &prepared.assembler.values(),
            &error,
        );
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
        publish_error(
            &prepared.assembler.diagnostic_bus(),
            &prepared.assembler.values(),
            &error,
        );
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
                assembler_diagnostics.publish_local(Diagnostic::new(
                    &assembler.values(),
                    Severity::Error,
                    format!("could not write stdout: {error}"),
                ));
            }
        }
        Err(error) => {
            operation_failed = true;
            publish_error(&assembler_diagnostics, &assembler.values(), &error);
        }
    }

    operation_failed |= finish_local_files(
        &local_files,
        manifest.as_deref(),
        &assembler_diagnostics,
        &assembler.values(),
    );
    let LoggerRun {
        thread: logger_thread,
        diagnostics: logger_diagnostics,
        supervisor: logger_supervisor,
    } = logger;
    operation_failed |= settle_batch_runtime(&assembler.evaluation_runtime(), &logger_supervisor);

    logger_thread.join().expect("logger task should not panic");
    if let Err(error) = logger_supervisor.deliver_fallback() {
        operation_failed = true;
        eprintln!("error: could not drain fallback diagnostics: {error}");
    }
    drop(logger_supervisor);

    if operation_failed
        || assembler_diagnostics.counts().errors() > 0
        || logger_diagnostics.counts().errors() > 0
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Pumps and settles the complete batch runtime, retaining report failures as
/// authoritative batch status independently of whether fallback rendering
/// succeeds.
///
/// A stable deadlock is explicitly killed and reported. Rendering may itself
/// admit runtime work, so each non-empty report is followed by another
/// complete pump/settlement cycle.
fn settle_batch_runtime(runtime: &EvaluationRuntime, supervisor: &LoggerSupervisor) -> bool {
    let mut failed = false;

    loop {
        runtime.pump_until_stable();
        if let Err(error) = supervisor.deliver_fallback() {
            failed = true;
            eprintln!("error: could not drain fallback diagnostics: {error}");
        }
        let snapshot = match runtime.readiness() {
            RuntimeReadiness::Busy => continue,
            RuntimeReadiness::Ready(snapshot) => snapshot,
            RuntimeReadiness::Deadlocked(deadlock) => deadlock.kill(RuntimeKillReason::Deadlock),
        };
        let mut report = match snapshot.settle() {
            Ok(report) => report,
            Err(_) => continue,
        };

        failed |= settled_report_is_fatal(&report);
        match supervisor.render_settled_report(&mut report) {
            Ok(0) => return failed,
            Ok(_) => {}
            Err(error) => {
                // The retained report remains authoritative. In particular, a
                // fallback-adapter failure becomes a delivery failure in the
                // next settled report even though this last-resort text cannot
                // rely on that same adapter.
                failed = true;
                eprintln!("error: could not render runtime settlement: {error}");
            }
        }
    }
}

fn settled_report_is_fatal(report: &QuiescenceReport) -> bool {
    !report.task_failures().is_empty()
        || !report.delivery_failures().failures().is_empty()
        || report
            .dispositions()
            .iter()
            .any(|disposition| matches!(disposition.kind(), RuntimeDispositionKind::ExitError(_)))
        || !report.killed_work().is_empty()
}

/// Settles a batch runtime when no configured logger lifecycle was started.
/// Runtime reports are committed directly to the assembler diagnostic bus and
/// are drained by the default logger after this function returns.
fn settle_batch_runtime_default(
    runtime: &EvaluationRuntime,
    diagnostics: &DiagnosticBus,
    values: &Values,
) -> bool {
    let mut failed = false;
    loop {
        runtime.pump_until_stable();
        let snapshot = match runtime.readiness() {
            RuntimeReadiness::Busy => continue,
            RuntimeReadiness::Ready(snapshot) => snapshot,
            RuntimeReadiness::Deadlocked(deadlock) => deadlock.kill(RuntimeKillReason::Deadlock),
        };
        let mut report = match snapshot.settle() {
            Ok(report) => report,
            Err(_) => continue,
        };
        failed |= settled_report_is_fatal(&report);
        let selection = SettledReportSelection {
            task_failures: report.pending_task_failure_reports().to_vec(),
            delivery_failures: report
                .pending_delivery_failure_reports()
                .failures()
                .into_iter()
                .collect(),
            exit_errors: report.pending_exit_error_reports().to_vec(),
            killed_work: report.pending_killed_work_reports().to_vec(),
        };
        let rendered = match settled_report_diagnostics(values, selection) {
            Ok(rendered) => rendered,
            Err(error) => {
                eprintln!("error: could not render runtime settlement: {error}");
                return true;
            }
        };
        let rendered_count = rendered.len();
        for diagnostic in rendered {
            diagnostics.publish_local(diagnostic);
        }
        report.mark_reports_enqueued();
        if rendered_count == 0 {
            return failed;
        }
    }
}

fn finish_without_logger(
    prepared: PreparedAssembly,
    manifest: Option<&Path>,
    operation_failed: bool,
) -> ExitCode {
    let diagnostics = prepared.assembler.diagnostic_bus();
    let runtime_failed = settle_batch_runtime_default(
        &prepared.assembler.evaluation_runtime(),
        &diagnostics,
        &prepared.assembler.values(),
    );
    let file_failure = finish_local_files(
        &prepared.local_files,
        manifest,
        &diagnostics,
        &prepared.assembler.values(),
    );
    prepared
        .log_host
        .drain_default(&DefaultLogger::new(prepared.assembler));
    if operation_failed || runtime_failed || file_failure || diagnostics.counts().errors() != 0 {
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
                    .publish_local(diagnostic.clone());
            }
            prepared.assembler.diagnostic_bus().publish_local(
                error
                    .diagnostic(&prepared.assembler.values())
                    .expect("configured CLI diagnostics belong to its assembler runtime"),
            );
            return finish_without_logger(prepared, None, true);
        }
    };
    let (command, diagnostics) = expansion.into_parts();
    for diagnostic in diagnostics {
        prepared
            .assembler
            .diagnostic_bus()
            .publish_local(diagnostic);
    }
    if let Some(nul_terminated) = inspection {
        let parts = command.into_parts();
        if let Err(error) = prepared.resolve_environment(&parts) {
            publish_error(
                &prepared.assembler.diagnostic_bus(),
                &prepared.assembler.values(),
                &error,
            );
            return finish_without_logger(prepared, None, true);
        }
        let output = format_configured_arguments(&parts.process_args, nul_terminated);
        let failed = if let Err(error) = io::stdout().write_all(&output) {
            prepared
                .assembler
                .diagnostic_bus()
                .publish_local(Diagnostic::new(
                    &prepared.assembler.values(),
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

fn assemble(
    assembler: &Assembler,
    inputs: Vec<ModuleInput>,
    cli_args: Vec<std::ffi::OsString>,
    environment: Value,
) -> Result<Bytes, Error> {
    let values = assembler.values();
    let arguments = values.list(
        cli_args
            .iter()
            .map(|argument| values.bytes(argument.as_encoded_bytes().to_vec())),
    )?;
    let initial_definitions = values.record([
        ("asm", values.record([("args", arguments)])?),
        ("env", environment),
    ])?;
    let module = assembler
        .module(["assembly"])
        .initial_definitions(initial_definitions)
        .inputs(inputs)
        .build()?;
    let context = assembly_result_context(&values)?;
    let result = values.access_names(module.value(), ["asm", "result"])?;
    values
        .anno_binary(result)
        .and_then(|binary| assembler.evaluator().eval(&binary))
        .and_then(|binary| {
            binary
                .as_bytes()
                .map(Bytes::copy_from_slice)
                .ok_or_else(|| Error::new("asm.result did not evaluate to binary data"))
        })
        .map_err(|error| {
            error
                .with_context(&values, context)
                .expect("assembly-result context belongs to the assembler runtime")
        })
}

fn assembly_result_context(values: &Values) -> Result<Value, Error> {
    values.record([(
        "asm",
        values.record([("result", values.text("asm.result"))])?,
    )])
}

struct LoadedConfiguration {
    value: Value,
    environment: Value,
}

fn load_configuration(assembler: &Assembler) -> Result<LoadedConfiguration, Error> {
    let default_environment = empty_environment_object(&assembler.values());
    let values = assembler.values();
    let initial_definitions = values.record([("env", default_environment.clone())])?;
    let module = assembler
        .module(["configuration"])
        .initial_definitions(initial_definitions)
        .inputs(configuration_paths().into_iter().map(ModuleInput::file))
        .build()?;

    let environment = values
        .access_names(module.value(), ["conf", "env"])
        .and_then(|candidate| with_path_lookup_context(&values, candidate, "conf.env"))
        .and_then(|candidate| {
            values.apply(
                &values.defined_or_function(),
                [default_environment, candidate],
            )
        })
        .and_then(|environment| assembler.evaluator().eval(&environment))
        .map(glam::EvaluatedValue::into_value)
        .map_err(|error| {
            error
                .with_context(
                    &values,
                    configuration_entry_context(&values, "env")
                        .expect("configuration context is local"),
                )
                .expect("configuration context is local")
        })?;
    Ok(LoadedConfiguration {
        value: module.into_value(),
        environment,
    })
}

fn start_logger(assembler: &Assembler, configuration: &Value, input: Arc<LogHost>) -> LoggerRun {
    let logger = Arc::new(DefaultLogger::new(assembler.clone()));
    let evaluation_runtime = assembler.evaluation_runtime();
    let fallback_logger = logger.clone();
    let supervisor = Arc::new(LoggerSupervisor::new(input.clone(), move |diagnostic| {
        fallback_logger.emit(&diagnostic);
    }));
    let diagnostics = DiagnosticBus::for_runtime(&evaluation_runtime);
    let subscription = diagnostics.subscribe(logger.clone());
    let host = Arc::new(LoggerTaskHost::new(
        input.clone(),
        diagnostics.clone(),
        assembler.reflection_environment_for_role("logger"),
        assembler.clone(),
    ));
    let effect_assembler = assembler.clone();
    let values = assembler.values();
    let custom = match values
        .access_names(configuration, ["conf", "log"])
        .and_then(|candidate| with_path_lookup_context(&values, candidate, "conf.log"))
        .and_then(|candidate| {
            values.apply(&values.defined_or_function(), [values.list([])?, candidate])
        })
        .and_then(|selected| assembler.evaluator().eval(&selected))
    {
        Ok(logger) if logger.array_items().is_some_and(|items| items.is_empty()) => None,
        Ok(logger) => Some(logger.into_value()),
        Err(error) => {
            let diagnostic = error
                .with_context(
                    &values,
                    configuration_entry_context(&values, "log")
                        .expect("configuration context is local"),
                )
                .and_then(|error| error.diagnostic(&values))
                .expect("configuration failure belongs to the assembler runtime");
            diagnostics.publish_local(diagnostic);
            None
        }
    };
    let task_diagnostics = diagnostics.clone();
    let task_values = evaluation_runtime.values();
    let scheduled = custom.and_then(|custom| {
        let installation = match supervisor.install() {
            Ok(installation) => installation,
            Err(error) => {
                publish_error(&task_diagnostics, &task_values, &error);
                return None;
            }
        };
        let run = EffectRun::new(
            &evaluation_runtime,
            &custom,
            MainEffects::new(effect_assembler),
            host,
        )
        .contextualizing_failures(
            configuration_entry_context(&task_values, "log")
                .expect("configuration context is local"),
        );
        match run.and_then(|run| {
            run.asserting_unit_result("configured logger result")
                .requiring_unit_result()
                .schedule_diagnostic_consumer(&installation.lifecycle, &input.diagnostic_ingress)
        }) {
            Ok(task) => Some((installation, task)),
            Err(error) => {
                supervisor.finish(&installation);
                task_diagnostics.publish_local(
                    error
                        .with_context(
                            configuration_entry_context(&task_values, "log")
                                .expect("configuration context is local"),
                        )
                        .diagnostic(&task_values),
                );
                None
            }
        }
    });
    let task_supervisor = supervisor.clone();
    let thread = thread::Builder::new()
        .name("glam-logger".to_owned())
        // The logger evaluates ordinary Glam configuration and therefore
        // needs the same practical stack headroom as the process main thread.
        // Deep evaluator paths are being migrated to explicit machines, but
        // the bootstrap must not make a configured logger depend on the
        // platform's smaller default child-thread stack in the meantime.
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let _subscription = subscription;
            if let Some((installation, task)) = scheduled {
                match task.run() {
                    Ok(TaskOutcome::Complete(_)) => {}
                    Ok(TaskOutcome::Cancelled) => {
                        task_diagnostics.publish_local(
                            Diagnostic::new(
                                &task_values,
                                Severity::Error,
                                "configured logger remained blocked after the log stream closed",
                            )
                            .with_context(
                                &task_values,
                                configuration_entry_context(&task_values, "log")
                                    .expect("configuration context is local"),
                            )
                            .expect("configuration context is local"),
                        );
                    }
                    Err(error) => {
                        if !matches!(
                            installation.lifecycle.status(),
                            glam::reflection::EffectLifecycleStatus::Failed(_)
                                | glam::reflection::EffectLifecycleStatus::Exited
                                | glam::reflection::EffectLifecycleStatus::Killed(_)
                        ) {
                            task_diagnostics.publish_local(error.diagnostic(&task_values));
                        }
                    }
                }
                task_supervisor.finish(&installation);
            } else {
                let _ = task_supervisor.fallback_and_deliver();
            }
        })
        .expect("logger thread should start");
    LoggerRun {
        thread,
        diagnostics,
        supervisor,
    }
}

fn configuration_entry_context(values: &Values, entry: &str) -> Result<Value, Error> {
    values.record([("conf", values.record([("entry", values.text(entry))])?)])
}

fn with_path_lookup_context(values: &Values, value: Value, path: &str) -> Result<Value, Error> {
    let frame = values.record([(
        "eval",
        values.record([
            ("op", values.atom_from_text("path_lookup")),
            ("args", values.record([("path", values.text(path))])?),
        ])?,
    )])?;
    values.anno(values.record([("context", frame)])?, value)
}

struct LoggerRun {
    thread: thread::JoinHandle<()>,
    diagnostics: DiagnosticBus,
    supervisor: Arc<LoggerSupervisor>,
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
    ReadLog,
    WriteStderr,
}

type MainSnapshot = RuntimeEventSnapshot;

#[derive(Clone, Default)]
struct MainJournal {
    reflection: ReflectionJournal,
    events: Option<RuntimeEventJournal>,
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
        .get_or_insert_with(|| RuntimeEventJournal::new(snapshot.clone()))
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
            MainRequest::ReadLog => read_log(context),
            MainRequest::WriteStderr => {
                let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
                    glam::reflection::TaskHalt::new(
                        "`.write_stderr` received the wrong number of arguments",
                    )
                })?;
                let values = self.assembler.values();
                let binary = values
                    .anno_binary(value)
                    .and_then(|binary| self.assembler.evaluator().eval(&binary))
                    .map_err(glam::reflection::TaskHalt::from)?;
                let bytes = binary
                    .as_bytes()
                    .map(Bytes::copy_from_slice)
                    .ok_or_else(|| {
                        glam::reflection::TaskHalt::new(
                            "`.write_stderr` argument did not evaluate to binary data",
                        )
                    })?;
                let stderr_writer = context.host().stderr_writer.clone();
                if let Some(mut transaction) = context.transaction() {
                    let (snapshot, journal) = transaction.parts();
                    event_journal(snapshot, journal)
                        .write(&stderr_writer, binary.into_value())
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

fn read_log(
    context: &mut RequestContext<'_, MainEffects>,
) -> Result<RequestResult, glam::reflection::TaskHalt> {
    let diagnostic_reader = context.host().diagnostic_reader.clone();
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
                .and_then(|diagnostic| diagnostic.enrich(&context.host().resources.values()))
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
        let mut events = RuntimeEventJournal::new(snapshot.extra().clone());
        let Some(value) = events
            .read(&diagnostic_reader)
            .map_err(glam::reflection::TaskHalt::from)?
        else {
            return Ok(RequestResult::Fail);
        };
        let value = Diagnostic::from_transport_value(&value)
            .and_then(|diagnostic| diagnostic.enrich(&context.host().resources.values()))
            .map_err(glam::reflection::TaskHalt::from)?;
        let commit = TaskCommit::new(
            glam::reflection::StoreJournal::new(snapshot.store().clone()),
            snapshot.extra().clone(),
            MainJournal {
                reflection: ReflectionJournal::default(),
                events: Some(events),
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
    task_capability: Arc<RuntimeTaskCapability>,
    diagnostic_ingress: DiagnosticIngress,
    diagnostic_reader: RuntimeInputReader,
}

/// Host ownership for one long-lived diagnostic ingress and a sequence of
/// configured logger lifecycles. Rearming changes only the coordinator root;
/// publications keep flowing through the original ingress and bus sequence.
struct LoggerSupervisor {
    input: Arc<LogHost>,
    fallback_writer: RuntimeOutputWriter,
    fallback_delivery: RuntimeOutputDelivery<Diagnostic>,
    state: std::sync::Mutex<LoggerSupervisorState>,
}

struct LoggerSupervisorState {
    next_generation: u64,
    active: Option<LoggerInstallation>,
}

struct SettledReportSelection {
    task_failures: Vec<glam::ReasoningFailure>,
    delivery_failures: Vec<Arc<RuntimeDeliveryFailure>>,
    exit_errors: Vec<RuntimeDisposition>,
    killed_work: Vec<RuntimeDeadlockWork>,
}

#[derive(Clone)]
struct LoggerInstallation {
    generation: u64,
    lifecycle: EffectLifecycle,
}

impl LoggerSupervisor {
    fn new<F>(input: Arc<LogHost>, fallback: F) -> Self
    where
        F: Fn(Diagnostic) + Send + Sync + 'static,
    {
        Self::new_fallible(input, move |diagnostic| {
            fallback(diagnostic);
            Ok(())
        })
    }

    fn new_fallible<F>(input: Arc<LogHost>, fallback: F) -> Self
    where
        F: Fn(Diagnostic) -> Result<(), Error> + Send + Sync + 'static,
    {
        let endpoint = input
            .runtime
            .output_endpoint(|value| Diagnostic::from_transport_value(&value), fallback)
            .expect("default logger output endpoint should be constructible");
        let (writer, fallback_delivery) = endpoint.into_parts();
        input
            .diagnostic_ingress
            .set_fallback_output(&writer)
            .expect("default logger output belongs to the logger runtime");
        Self {
            input,
            fallback_writer: writer,
            fallback_delivery,
            state: std::sync::Mutex::new(LoggerSupervisorState {
                next_generation: 1,
                active: None,
            }),
        }
    }

    fn install(&self) -> Result<LoggerInstallation, Error> {
        let mut state = self
            .state
            .lock()
            .expect("logger supervisor mutex should not be poisoned");
        if state
            .active
            .as_ref()
            .is_some_and(|active| !active.lifecycle.status().is_terminal())
        {
            return Err(Error::new("configured logger lifecycle is still active"));
        }
        let generation = state.next_generation;
        state.next_generation = generation
            .checked_add(1)
            .expect("logger lifecycle generations exhausted");
        let fallback_delivery = self.fallback_delivery.clone();
        let terminal = self.input.diagnostic_ingress.logger_terminal(move || {
            let _ = deliver_fallback_output(&fallback_delivery);
        });
        let installation = LoggerInstallation {
            generation,
            lifecycle: EffectLifecycle::new_with_terminal(&self.input.runtime, terminal),
        };
        state.active = Some(installation.clone());
        Ok(installation)
    }

    fn finish(&self, installation: &LoggerInstallation) {
        let mut state = self
            .state
            .lock()
            .expect("logger supervisor mutex should not be poisoned");
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == installation.generation)
        {
            state.active = None;
        }
    }

    fn fallback_and_deliver(&self) -> Result<usize, Error> {
        let transferred = self.input.diagnostic_ingress.fallback()?;
        self.deliver_fallback()?;
        Ok(transferred)
    }

    fn deliver_fallback(&self) -> Result<(), Error> {
        deliver_fallback_output(&self.fallback_delivery)
    }

    fn render_settled_report(&self, report: &mut QuiescenceReport) -> Result<usize, Error> {
        if report.runtime_id() != self.input.runtime.id() {
            return Err(Error::new(format!(
                "quiescence report belongs to evaluation runtime {}, not {}",
                report.runtime_id().get(),
                self.input.runtime.id().get()
            )));
        }
        let selected = SettledReportSelection {
            task_failures: report.pending_task_failure_reports().to_vec(),
            delivery_failures: report
                .pending_delivery_failure_reports()
                .failures()
                .into_iter()
                .filter(|failure| failure.endpoint_id() != self.fallback_delivery.id())
                .collect(),
            exit_errors: report.pending_exit_error_reports().to_vec(),
            killed_work: report.pending_killed_work_reports().to_vec(),
        };
        let values = self.input.runtime.values();
        let diagnostics = settled_report_diagnostics(&values, selected)?;
        let rendered = diagnostics.len();
        self.enqueue_fallback_diagnostics(diagnostics)?;
        // Enqueue is the report transport's transactional commitment point.
        // Delivery may still fail, but replaying an accepted outbox batch would
        // duplicate externally visible output.
        report.mark_reports_enqueued();
        self.deliver_fallback()?;
        Ok(rendered)
    }

    fn enqueue_fallback_diagnostics(&self, diagnostics: Vec<Diagnostic>) -> Result<(), Error> {
        if diagnostics.is_empty() {
            return Ok(());
        }
        let values = self.input.runtime.values();
        let payloads = diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.transport_value(&values))
            .collect::<Result<Vec<_>, _>>()?;
        loop {
            let (_generation, store, snapshot) = self.input.task_capability.transaction_snapshot();
            let mut events = RuntimeEventJournal::new(snapshot);
            for payload in &payloads {
                events.write(&self.fallback_writer, payload.clone())?;
            }
            match self
                .input
                .task_capability
                .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events)
            {
                glam::reflection::StoreCommitResult::Committed => return Ok(()),
                glam::reflection::StoreCommitResult::Conflict => {}
                glam::reflection::StoreCommitResult::MissingVolume(volume) => {
                    return Err(Error::new(format!(
                        "reflection volume {} was revoked while reporting runtime settlement",
                        volume.get()
                    )));
                }
            }
        }
    }

    #[cfg(test)]
    fn active_status(&self) -> Option<glam::reflection::EffectLifecycleStatus> {
        self.state
            .lock()
            .expect("logger supervisor mutex should not be poisoned")
            .active
            .as_ref()
            .map(|active| active.lifecycle.status())
    }
}

fn settled_report_diagnostics(
    values: &Values,
    selection: SettledReportSelection,
) -> Result<Vec<Diagnostic>, Error> {
    let mut diagnostics = Vec::new();
    for failure in selection.task_failures {
        let context = runtime_report_context(
            values,
            "task_failure",
            vec![
                ("session", report_id(values, failure.session_id())?),
                ("task", report_id(values, failure.task_id())?),
            ],
        )?;
        diagnostics.push(failure.diagnostic().clone().with_context(values, context)?);
    }
    for failure in selection.delivery_failures {
        let context = runtime_report_context(
            values,
            "delivery_failure",
            vec![
                ("delivery", report_id(values, failure.delivery_id().get())?),
                ("endpoint", report_id(values, failure.endpoint_id().get())?),
                (
                    "kind",
                    values.atom_from_text(match failure.kind() {
                        glam::RuntimeDeliveryFailureKind::Decode => "decode",
                        glam::RuntimeDeliveryFailureKind::Adapter => "adapter",
                        glam::RuntimeDeliveryFailureKind::Panic => "panic",
                    }),
                ),
            ],
        )?;
        diagnostics.push(
            failure
                .error()
                .diagnostic(values)?
                .with_context(values, context)?,
        );
    }
    for disposition in selection.exit_errors {
        let RuntimeDispositionKind::ExitError(message) = disposition.kind() else {
            unreachable!("report selection retains only error exits")
        };
        let mut args = vec![
            ("work", report_id(values, disposition.work_id())?),
            ("session", report_id(values, disposition.session_id())?),
        ];
        if let Some(task) = disposition.task_id() {
            args.push(("task", report_id(values, task)?));
        }
        let context = runtime_report_context(values, "exit_error", args)?;
        diagnostics.push(
            Diagnostic::from_emission(Severity::Error, message.clone())
                .with_context(values, context)?,
        );
    }
    for work in selection.killed_work {
        let blocked_error = work
            .project_blocked_diagnostic(values)?
            .map(|diagnostic| diagnostic.message().to_owned());
        let mut args = vec![
            ("work", report_id(values, work.work_id())?),
            ("session", report_id(values, work.session_id())?),
            (
                "kind",
                values.atom_from_text(runtime_work_kind_name(work.kind())),
            ),
            (
                "state",
                values.atom_from_text(runtime_work_state_name(work.state())),
            ),
        ];
        if let Some(task) = work.task_id() {
            args.push(("task", report_id(values, task)?));
        }
        if let Some(epoch) = work.observed_epoch() {
            args.push(("observed_epoch", report_id(values, epoch)?));
        }
        if let Some(dependency) = work.dependency() {
            args.push(("dependency", runtime_dependency_value(values, dependency)?));
        }
        let context = runtime_report_context(values, "killed", args)?;
        let mut message = format!(
            "{} deadlocked; runtime killed {} work {} in settlement",
            if work.kind() == RuntimeWorkKind::ReflectionTask {
                "reflection scheduler"
            } else {
                "evaluation runtime"
            },
            runtime_work_kind_name(work.kind()),
            work.work_id()
        );
        if let Some(blocked) = &blocked_error {
            message.push_str("; retained error: ");
            message.push_str(blocked);
        }
        diagnostics
            .push(Diagnostic::new(values, Severity::Error, message).with_context(values, context)?);
    }
    Ok(diagnostics)
}

fn report_id(values: &Values, id: u64) -> Result<Value, Error> {
    values.number_from_text(id.to_string())
}

fn runtime_report_context(
    values: &Values,
    operation: &str,
    args: Vec<(&str, Value)>,
) -> Result<Value, Error> {
    values.record([(
        "runtime",
        values.record([
            ("op", values.atom_from_text(operation)),
            ("args", values.record(args)?),
        ])?,
    )])
}

fn runtime_work_kind_name(kind: RuntimeWorkKind) -> &'static str {
    match kind {
        RuntimeWorkKind::ReflectionTask => "reflection_task",
        RuntimeWorkKind::DeferredEvaluation => "deferred_evaluation",
        RuntimeWorkKind::ClientDemand => "client_demand",
        RuntimeWorkKind::Spark => "spark",
    }
}

fn runtime_work_state_name(state: RuntimeWorkState) -> &'static str {
    match state {
        RuntimeWorkState::Dormant => "dormant",
        RuntimeWorkState::Reserved => "reserved",
        RuntimeWorkState::Blocked => "blocked",
    }
}

fn runtime_dependency_value(
    values: &Values,
    dependency: &RuntimeDependency,
) -> Result<Value, Error> {
    match dependency {
        RuntimeDependency::TaskWait {
            wait_id,
            task_id,
            session_id,
        } => values.record([(
            "task_wait",
            values.record([
                ("wait", report_id(values, *wait_id)?),
                ("task", report_id(values, *task_id)?),
                ("session", report_id(values, *session_id)?),
            ])?,
        )]),
        RuntimeDependency::Promise {
            promise_id,
            producer,
        } => {
            let mut fields = vec![("promise", report_id(values, *promise_id)?)];
            if let Some(producer) = producer {
                fields.push((
                    "producer",
                    values.record([
                        ("wait", report_id(values, producer.wait_id())?),
                        ("task", report_id(values, producer.task_id())?),
                        ("session", report_id(values, producer.session_id())?),
                    ])?,
                ));
            }
            values.record([("promise", values.record(fields)?)])
        }
        RuntimeDependency::Synthetic { id } => values.record([(
            "synthetic",
            values.record([("id", report_id(values, *id)?)])?,
        )]),
    }
}

fn deliver_fallback_output(
    fallback_delivery: &RuntimeOutputDelivery<Diagnostic>,
) -> Result<(), Error> {
    loop {
        match fallback_delivery.deliver_next()? {
            Some(RuntimeDeliveryOutcome::Delivered(_)) => {}
            Some(RuntimeDeliveryOutcome::Failed(failure)) => {
                return Err(failure.error().clone());
            }
            None => return Ok(()),
        }
    }
}

/// Capabilities and mutable state belonging to the logger's evaluation
/// session. Incoming assembler diagnostics remain in the runtime input queue;
/// diagnostics emitted by this session go only to its diagnostic bus.
struct LoggerTaskHost {
    resources: Arc<RuntimeTaskCapability>,
    _diagnostic_ingress: DiagnosticIngress,
    diagnostic_reader: RuntimeInputReader,
    diagnostics: DiagnosticBus,
    reflection_environment: Value,
    diagnostic_writer: RuntimeOutputWriter,
    diagnostic_delivery: RuntimeOutputDelivery<Diagnostic>,
    stderr_writer: RuntimeOutputWriter,
    stderr_delivery: RuntimeOutputDelivery<Bytes>,
}

impl LoggerTaskHost {
    fn new(
        input: Arc<LogHost>,
        diagnostics: DiagnosticBus,
        reflection_environment: Value,
        assembler: Assembler,
    ) -> Self {
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
                move |value| {
                    assembler
                        .evaluator()
                        .eval(&value)?
                        .as_bytes()
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
            resources: input.task_capability.clone(),
            _diagnostic_ingress: input.diagnostic_ingress.clone(),
            diagnostic_reader: input.diagnostic_reader.clone(),
            diagnostics,
            reflection_environment,
            diagnostic_writer,
            diagnostic_delivery,
            stderr_writer,
            stderr_delivery,
        }
    }

    fn write_diagnostic(&self, diagnostic: Diagnostic) -> Result<(), Error> {
        let (_generation, store, snapshot) = self.resources.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events.write(
            &self.diagnostic_writer,
            diagnostic.transport_value(&self.resources.values())?,
        )?;
        match self
            .resources
            .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events)
        {
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
        let (_generation, store, snapshot) = self.resources.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events.write(&self.stderr_writer, self.resources.values().bytes(bytes))?;
        match self
            .resources
            .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events)
        {
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
        let task_capability = runtime.task_capability();
        let (ingress, diagnostic_reader) = diagnostics
            .diagnostic_ingress(&runtime)
            .expect("logger diagnostic ingress should be constructible");
        Self {
            runtime,
            task_capability,
            diagnostic_ingress: ingress,
            diagnostic_reader,
        }
    }

    fn drain_default(&self, logger: &DefaultLogger) {
        while let Some(diagnostic) = self.take_diagnostic() {
            logger.emit(&diagnostic);
        }
    }

    fn take_diagnostic(&self) -> Option<Diagnostic> {
        loop {
            let (_generation, store, snapshot) = self.task_capability.transaction_snapshot();
            let mut events = RuntimeEventJournal::new(snapshot);
            let value = events
                .read(&self.diagnostic_reader)
                .expect("logger diagnostic endpoint should match its runtime");
            if let Some(value) = value {
                match self
                    .task_capability
                    .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events)
                {
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
            return None;
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
            self.diagnostics.publish_local(
                error
                    .diagnostic(&self.resources.values())
                    .expect("logger failures belong to the logger runtime"),
            );
        }
    }

    fn query_writer(&self) -> Option<Arc<dyn ReflectionQueryWriter>> {
        Some(self.resources.clone())
    }
}

impl TaskHost<MainEffects> for LoggerTaskHost {
    fn snapshot(&self) -> HostSnapshot<MainEffects> {
        let (generation, store, input) = self.resources.transaction_snapshot();
        HostSnapshot::new(generation, store, input)
    }

    fn commit(&self, commit: TaskCommit<MainEffects>) -> CommitResult {
        let (store, snapshot, journal) = commit.into_parts();
        let mut events = journal
            .events
            .unwrap_or_else(|| RuntimeEventJournal::new(snapshot.clone()));
        for diagnostic in journal.reflection.diagnostics() {
            if let Err(error) = events.write(
                &self.diagnostic_writer,
                diagnostic
                    .transport_value(&self.resources.values())
                    .map_err(|_| ())
                    .unwrap_or_else(|()| {
                        self.resources
                            .values()
                            .record([(
                                "emission",
                                self.resources.values().text("diagnostic transport failed"),
                            )])
                            .expect("fallback diagnostic is local")
                    }),
            ) {
                self.diagnostics.publish_local(
                    error
                        .diagnostic(&self.resources.values())
                        .expect("logger failures belong to the logger runtime"),
                );
                return CommitResult::Closed;
            }
        }
        match self.resources.try_commit_transaction(&store, &events) {
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
            self.diagnostics.publish_local(
                error
                    .diagnostic(&self.resources.values())
                    .expect("logger failures belong to the logger runtime"),
            );
        }
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        self.resources.wait_for_change(observed_generation)
    }
}

fn empty_environment_object(values: &glam::Values) -> Value {
    values
        .empty_object(values.abstract_global_path(["configuration", "env"]))
        .expect("empty environment components belong to one runtime")
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
        let message =
            Diagnostic::apply_updates(&values, &message, self.context_lines_update(context_lines))?;
        self.format_message(message)
    }

    fn format_message(&self, message: Value) -> Result<Bytes, Error> {
        let values = self.evaluator.values();
        let rendered = values.apply(&self.formatter, [message])?;
        let binary = values.anno_binary(rendered)?;
        let evaluated = self.evaluator.evaluator().eval(&binary)?;
        evaluated
            .as_bytes()
            .map(Bytes::copy_from_slice)
            .ok_or_else(|| Error::new("diagnostic formatter did not return binary data"))
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
        let values = self.evaluator.values();
        let mut viewer = vec![
            ("kind", values.text("terminal")),
            (
                "columns",
                values.integer(i64::try_from(terminal.columns).unwrap_or(i64::MAX)),
            ),
            ("color", values.text(terminal.color.name())),
            ("header", values.text(header)),
            ("auto_indent", values.integer(Self::AUTO_INDENT as i64)),
            (
                "indent",
                values.text(" ".repeat(base_indent + Self::AUTO_INDENT)),
            ),
            (
                "anchor_indent",
                values.text(" ".repeat(base_indent + Self::ANCHOR_INDENT)),
            ),
            ("location", values.text(location)),
            (
                "context_lines",
                values
                    .list(std::iter::empty())
                    .expect("empty list is local"),
            ),
        ];
        if let Some(term) = &terminal.term {
            viewer.push(("term", values.text(term)));
        }
        if let Some(language) = &terminal.language {
            viewer.push(("lang", values.text(language)));
        }
        if let Some(source) = source {
            viewer.push((
                "source",
                values
                    .record([("file", values.text(source))])
                    .expect("source viewer value is local"),
            ));
        }
        values
            .record([(
                "viewer",
                values.record(viewer).expect("viewer fields are local"),
            )])
            .expect("viewer update is local")
    }

    fn context_lines(
        &self,
        message: &Value,
        terminal: &TerminalContext,
        base_indent: usize,
    ) -> Vec<String> {
        let values = self.evaluator.values();
        let frames = match values
            .access_names(message, ["msg", "context"])
            .and_then(|candidate| {
                values.apply(&values.defined_or_function(), [values.list([])?, candidate])
            })
            .and_then(|contexts| values.anno_array(contexts))
            .and_then(|array| self.evaluator.evaluator().eval(&array))
            .and_then(|array| {
                array
                    .array_items()
                    .ok_or_else(|| glam::Error::new("context array did not materialize"))
            }) {
            Ok(frames) => frames,
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
        let frame = match self.evaluator.evaluator().eval(frame) {
            Ok(frame) => frame.into_value(),
            Err(error) => {
                return format!(
                    "{}msg: <context rendering failed: {error}>",
                    " ".repeat(frame_indent)
                );
            }
        };
        let message_tag = self.evaluator.values().atom_from_text("msg");
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
            Diagnostic::apply_updates(&values, &message, self.viewer_header_update(header))?
        };
        let context_lines = self.context_lines(&message, terminal, frame_indent);
        let message =
            Diagnostic::apply_updates(&values, &message, self.context_lines_update(context_lines))?;
        let rendered = self.format_message(message)?;
        let rendered = String::from_utf8_lossy(&rendered);
        let rendered = rendered.strip_suffix('\n').unwrap_or(&rendered);
        Ok(format!("{}{rendered}", " ".repeat(frame_indent)))
    }

    fn context_message_header(&self, message: &Value, terminal: &TerminalContext) -> String {
        let values = self.evaluator.values();
        let Ok(severity) = values
            .access_names(message, ["msg", "severity"])
            .and_then(|severity| self.evaluator.evaluator().eval(&severity))
        else {
            return "msg: ".to_owned();
        };
        let Ok(key) = self.evaluator.reflection().atom_key(severity.as_value()) else {
            return "msg: ".to_owned();
        };
        match diagnostic_text(&self.evaluator, &key).as_deref() {
            Some("info") => Self::severity_header(Severity::Info, terminal),
            Some("warn") => Self::severity_header(Severity::Warning, terminal),
            Some("error") => Self::severity_header(Severity::Error, terminal),
            _ => "msg: ".to_owned(),
        }
    }

    fn viewer_header_update(&self, header: String) -> Value {
        let values = self.evaluator.values();
        values
            .record([(
                "viewer",
                values
                    .record([("header", values.text(header))])
                    .expect("viewer header is local"),
            )])
            .expect("viewer update is local")
    }

    fn context_lines_update(&self, lines: Vec<String>) -> Value {
        let values = self.evaluator.values();
        let lines = values
            .list(lines.into_iter().map(|line| values.text(line)))
            .expect("context lines are local");
        values
            .record([(
                "viewer",
                values
                    .record([("context_lines", lines)])
                    .expect("context-line viewer field is local"),
            )])
            .expect("viewer update is local")
    }

    fn summarize_context_frame(&self, frame: &Value) -> String {
        let reflection = self.evaluator.reflection();
        let Ok(entries) = reflection.dictionary_items(frame) else {
            return diagnostic_value_kind(&self.evaluator, frame).to_owned();
        };
        let [(tag, payload)] = entries.as_slice() else {
            return diagnostic_value_kind(&self.evaluator, frame).to_owned();
        };

        let values = self.evaluator.values();
        if tag == &values.atom_from_text("eval") {
            return self.eval_context_summary(payload);
        }
        if tag == &values.atom_from_text("g") {
            return self.g_context_summary(payload);
        }
        if tag == &values.atom_from_text("import") {
            return self.import_context_summary(payload);
        }
        if tag == &values.atom_from_text("asm") {
            return self.asm_context_summary(payload);
        }
        if tag == &values.atom_from_text("conf") {
            return self.conf_context_summary(payload);
        }
        if tag == &values.atom_from_text("task") {
            return self.task_context_summary(payload);
        }
        if tag == &values.atom_from_text("runtime") {
            return self.runtime_context_summary(payload);
        }
        self.context_tag_text(tag)
            .unwrap_or_else(|| diagnostic_value_kind(&self.evaluator, frame).to_owned())
    }

    fn eval_context_summary(&self, payload: &Value) -> String {
        let operation = self
            .context_field_tag_text(payload, &["op"])
            .map(|operation| operation.replace('_', " "));
        let path = self.context_field_text(payload, &["args", "path"]);
        match (operation, path) {
            (Some(operation), Some(path)) => format!("eval: {operation} `{path}`"),
            (Some(operation), None) => format!("eval: {operation}"),
            (None, Some(path)) => format!("eval: path `{path}`"),
            (None, None) => "eval".to_owned(),
        }
    }

    fn g_context_summary(&self, payload: &Value) -> String {
        let definition = self.context_field_text(payload, &["definition"]);
        let line = self.context_field_text(payload, &["line"]);
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
        self.context_field_text(payload, &["request", "file"])
            .map_or_else(
                || "import".to_owned(),
                |request| format!("import: request `{request}`"),
            )
    }

    fn asm_context_summary(&self, payload: &Value) -> String {
        self.context_field_text(payload, &["result"]).map_or_else(
            || "asm".to_owned(),
            |result| format!("asm: result `{result}`"),
        )
    }

    fn conf_context_summary(&self, payload: &Value) -> String {
        self.context_field_text(payload, &["entry"]).map_or_else(
            || "conf".to_owned(),
            |entry| format!("conf: entry `{entry}`"),
        )
    }

    fn task_context_summary(&self, payload: &Value) -> String {
        let operation = self.context_field_tag_text(payload, &["operation"]);
        let id = self.context_field_text(payload, &["id"]);
        match (operation, id) {
            (Some(operation), Some(id)) => format!("task: {operation} task {id}"),
            (Some(operation), None) => format!("task: {operation}"),
            (None, Some(id)) => format!("task: task {id}"),
            (None, None) => "task".to_owned(),
        }
    }

    fn runtime_context_summary(&self, payload: &Value) -> String {
        let operation = self
            .context_field_tag_text(payload, &["op"])
            .map(|operation| operation.replace('_', " "));
        let work = self.context_field_text(payload, &["args", "work"]);
        let session = self.context_field_text(payload, &["args", "session"]);
        let task = self.context_field_text(payload, &["args", "task"]);
        let delivery = self.context_field_text(payload, &["args", "delivery"]);
        let endpoint = self.context_field_text(payload, &["args", "endpoint"]);
        let kind = self
            .context_field_tag_text(payload, &["args", "kind"])
            .map(|kind| kind.replace('_', " "));

        let mut details = Vec::new();
        if let Some(work) = work {
            details.push(format!("work {work}"));
        }
        if let Some(session) = session {
            details.push(format!("session {session}"));
        }
        if let Some(task) = task {
            details.push(format!("task {task}"));
        }
        if let Some(delivery) = delivery {
            details.push(format!("delivery {delivery}"));
        }
        if let Some(endpoint) = endpoint {
            details.push(format!("endpoint {endpoint}"));
        }
        if let Some(kind) = kind {
            details.push(kind);
        }

        let operation = operation.unwrap_or_else(|| "event".to_owned());
        if details.is_empty() {
            format!("runtime: {operation}")
        } else {
            format!("runtime: {operation} ({})", details.join(", "))
        }
    }

    fn context_field_text(&self, value: &Value, path: &[&str]) -> Option<String> {
        self.evaluator
            .values()
            .access_names(value, path.iter().copied())
            .ok()
            .and_then(|value| diagnostic_text(&self.evaluator, &value))
    }

    fn context_field_tag_text(&self, value: &Value, path: &[&str]) -> Option<String> {
        self.evaluator
            .values()
            .access_names(value, path.iter().copied())
            .ok()
            .and_then(|value| self.context_tag_text(&value))
    }

    fn context_tag_text(&self, tag: &Value) -> Option<String> {
        diagnostic_text(&self.evaluator, tag).or_else(|| {
            self.evaluator
                .reflection()
                .atom_key(tag)
                .ok()
                .and_then(|key| diagnostic_text(&self.evaluator, &key))
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

fn diagnostic_text(assembler: &Assembler, value: &Value) -> Option<String> {
    let value = assembler.evaluator().eval(value).ok()?;
    value
        .as_bytes()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .or_else(|| value.number_text())
}

fn diagnostic_value_kind(assembler: &Assembler, value: &Value) -> &'static str {
    let values = assembler.values();
    if value == &values.abstract_global_path(["builtin", "unit"]) {
        return "Unit";
    }
    match assembler.reflection().kind(value) {
        Err(_) => "Foreign",
        Ok(glam::ValueKind::Atom) => "Atom",
        Ok(glam::ValueKind::Number) => "Number",
        Ok(glam::ValueKind::Binary) => "Binary",
        Ok(glam::ValueKind::List) => "List",
        Ok(glam::ValueKind::Dict) => {
            if assembler
                .reflection()
                .dictionary_items(value)
                .is_ok_and(|items| items.is_empty())
            {
                "Undefined"
            } else {
                "Dict"
            }
        }
        Ok(glam::ValueKind::Function) => "Function",
        Ok(glam::ValueKind::Net) => "Net",
        Ok(glam::ValueKind::Lazy) => "Lazy",
        Ok(glam::ValueKind::Sealed) => "Sealed",
        Ok(glam::ValueKind::Opaque) => "Opaque",
        Ok(_) => "Value",
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

    trait TestValueFacade {
        fn get(&self, root: &Value, path: &str) -> Result<Value, Error>;
        fn get_evaluated(&self, root: &Value, path: &str) -> Result<glam::EvaluatedValue, Error>;
    }

    impl TestValueFacade for Assembler {
        fn get(&self, root: &Value, path: &str) -> Result<Value, Error> {
            self.get_evaluated(root, path)
                .map(glam::EvaluatedValue::into_value)
        }

        fn get_evaluated(&self, root: &Value, path: &str) -> Result<glam::EvaluatedValue, Error> {
            let value = self.values().access_names(root, path.split('.'))?;
            self.evaluator().eval(&value)
        }
    }

    fn record<I, S>(values: &glam::Values, entries: I) -> Value
    where
        I: IntoIterator<Item = (S, Value)>,
        S: AsRef<str>,
    {
        values.record(entries).expect("test record should be local")
    }

    fn list(values: &glam::Values, items: impl IntoIterator<Item = Value>) -> Value {
        values.list(items).expect("test list should be local")
    }

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
        let values = queue.runtime.values();

        assert!(!finish_local_files(&files, None, &diagnostics, &values));
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
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(&values, Severity::Warning, "first\nsecond\n\nfourth")
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
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Error,
            record(
                &values,
                [(
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("broken\nmore detail")),
                            (
                                "context",
                                list(
                                    &values,
                                    [
                                        record(
                                            &values,
                                            [(
                                                "eval",
                                                record(
                                                    &values,
                                                    [(
                                                        "op",
                                                        values.atom_from_text("binary_extraction"),
                                                    )],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "g",
                                                record(
                                                    &values,
                                                    [
                                                        ("definition", values.text("result")),
                                                        ("line", values.integer(7)),
                                                    ],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "import",
                                                record(
                                                    &values,
                                                    [(
                                                        "request",
                                                        record(
                                                            &values,
                                                            [("file", values.text("child.g"))],
                                                        ),
                                                    )],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "asm",
                                                record(
                                                    &values,
                                                    [("result", values.text("asm.result"))],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "eval",
                                                record(
                                                    &values,
                                                    [
                                                        (
                                                            "op",
                                                            values.atom_from_text("path_lookup"),
                                                        ),
                                                        (
                                                            "args",
                                                            record(
                                                                &values,
                                                                [("path", values.text("conf.env"))],
                                                            ),
                                                        ),
                                                    ],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "conf",
                                                record(&values, [("entry", values.text("log"))]),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "task",
                                                record(
                                                    &values,
                                                    [
                                                        (
                                                            "operation",
                                                            values.atom_from_text("join"),
                                                        ),
                                                        ("id", values.integer(12)),
                                                    ],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "runtime",
                                                record(
                                                    &values,
                                                    [
                                                        (
                                                            "op",
                                                            values
                                                                .atom_from_text("delivery_failure"),
                                                        ),
                                                        (
                                                            "args",
                                                            record(
                                                                &values,
                                                                [
                                                                    (
                                                                        "delivery",
                                                                        values.integer(13),
                                                                    ),
                                                                    ("endpoint", values.integer(4)),
                                                                    (
                                                                        "kind",
                                                                        values.atom_from_text(
                                                                            "adapter",
                                                                        ),
                                                                    ),
                                                                ],
                                                            ),
                                                        ),
                                                    ],
                                                ),
                                            )],
                                        ),
                                    ],
                                ),
                            ),
                        ],
                    ),
                )],
            ),
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
                b"error: broken\n    more detail\n  context:\n    eval: binary extraction\n    g: definition `result` on line 7\n    import: request `child.g`\n    asm: result `asm.result`\n    eval: path lookup `conf.env`\n    conf: entry `log`\n    task: join task 12\n    runtime: delivery failure (delivery 13, endpoint 4, adapter)\n"
            )
        );
    }

    #[test]
    fn glam_default_formatter_recursively_renders_context_messages() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Error,
            record(&values, [(
                "msg",
                record(&values, [
                    ("text", values.text("outer failure")),
                    (
                        "context",
                        list(&values, [
                            record(&values, [(
                                "msg",
                                record(&values, [("text", values.text("unclassified context"))]),
                            )]),
                            record(&values, [(
                                "msg",
                                record(&values, [
                                    ("text", values.text("nested context\nmore detail")),
                                    ("severity", values.atom_from_text("info")),
                                    (
                                        "context",
                                        list(&values, [record(&values, [(
                                            "eval",
                                            record(&values, [(
                                                "op",
                                                values.atom_from_text("list_index"),
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
        let values = evaluator.values();
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
            record(
                &values,
                [(
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("outer failure")),
                            ("context", list(&values, [frame])),
                        ],
                    ),
                )],
            ),
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
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Error,
            record(
                &values,
                [(
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("outer failure")),
                            (
                                "context",
                                list(
                                    &values,
                                    [record(
                                        &values,
                                        [("msg", record(&values, [("text", values.integer(42))]))],
                                    )],
                                ),
                            ),
                        ],
                    ),
                )],
            ),
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
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            Severity::Warning,
            record(
                &values,
                [(
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("careful")),
                            (
                                "context",
                                list(
                                    &values,
                                    [
                                        record(&values, [("custom", values.integer(42))]),
                                        record(
                                            &values,
                                            [
                                                ("left", values.integer(1)),
                                                ("right", values.integer(2)),
                                            ],
                                        ),
                                        values.integer(7),
                                    ],
                                ),
                            ),
                        ],
                    ),
                )],
            ),
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
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(&values, Severity::Error, "broken");
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
        let diagnostic = Diagnostic::new(&logger.evaluator.values(), Severity::Info, "hello");
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
                .get_evaluated(&enriched, "viewer.auto_indent")
                .expect("viewer should declare automatic indentation")
                .as_i64(),
            Some(4)
        );
        assert_eq!(
            logger
                .evaluator
                .get_evaluated(&enriched, "viewer.header")
                .expect("viewer should materialize the complete message header")
                .as_bytes(),
            Some(b"\x1b[36minfo\x1b[0m: ".as_slice())
        );
        assert_eq!(
            logger
                .evaluator
                .get_evaluated(&enriched, "viewer.anchor_indent")
                .expect("viewer should expose its section anchor indentation")
                .as_bytes(),
            Some(b"  ".as_slice())
        );
        assert_eq!(
            logger
                .evaluator
                .get_evaluated(&enriched, "viewer.term")
                .expect("viewer should declare its terminal")
                .as_bytes(),
            Some(b"xterm-256color".as_slice())
        );
        let viewer = logger
            .evaluator
            .get_evaluated(diagnostic.emission(), "viewer")
            .expect("the raw diagnostic should expose undefined viewer metadata");
        assert!(
            logger
                .evaluator
                .reflection()
                .dictionary_items(viewer.as_value())
                .is_ok_and(|items| items.is_empty())
        );
    }

    #[test]
    fn assembly_result_context_names_the_executable_output_boundary() {
        let assembler = Assembler::default();
        assert_eq!(
            assembler
                .get_evaluated(
                    &assembly_result_context(&assembler.values())
                        .expect("result context should be local"),
                    "asm.result",
                )
                .expect("assembly result context should identify its output")
                .as_bytes(),
            Some(b"asm.result".as_slice())
        );
    }

    #[test]
    fn bus_error_count_survives_absent_subscribers_and_queue_reads() {
        let diagnostics = DiagnosticBus::new();
        let retained = Arc::new(LogHost::new(&diagnostics));
        let values = retained.runtime.values();
        diagnostics.publish_local(Diagnostic::new(&values, Severity::Error, "dropped"));
        assert_eq!(diagnostics.counts().errors(), 1);

        diagnostics.publish_local(Diagnostic::new(&values, Severity::Error, "retained"));
        assert!(retained.take_diagnostic().is_some());
        assert_eq!(diagnostics.counts().errors(), 2);
    }

    #[test]
    fn logger_supervisor_rearms_without_replacing_its_diagnostic_ingress() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let fallback = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fallback_values = fallback.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            fallback_values
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .push(diagnostic.message().to_owned());
        });
        let values = input.runtime.values();

        let first = supervisor.install().expect("first logger should install");
        assert_eq!(
            supervisor.active_status(),
            Some(glam::reflection::EffectLifecycleStatus::Launched)
        );
        let first_event =
            diagnostics.publish_local(Diagnostic::new(&values, Severity::Info, "first lifecycle"));
        supervisor.finish(&first);
        assert_eq!(
            supervisor
                .fallback_and_deliver()
                .expect("finished lifecycle should select fallback"),
            1
        );
        assert_eq!(
            *fallback
                .lock()
                .expect("fallback collection mutex should not be poisoned"),
            ["first lifecycle"]
        );

        let second = supervisor.install().expect("second logger should rearm");
        assert!(second.generation > first.generation);
        // Production rearm couples this transition to coordinator-root
        // activation. This test isolates the ingress identity across two
        // supervisor generations.
        input
            .diagnostic_ingress
            .activate()
            .expect("original ingress should rearm");
        let second_event =
            diagnostics.publish_local(Diagnostic::new(&values, Severity::Info, "second lifecycle"));
        assert_eq!(second_event.sequence(), first_event.sequence() + 1);
        assert_eq!(
            input
                .take_diagnostic()
                .expect("rearmed publication should use the original ingress")
                .message(),
            "second lifecycle"
        );
        assert!(input.take_diagnostic().is_none());
    }

    #[test]
    fn logger_exit_vote_retries_new_input_before_terminal_fallback() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("logger retry assembler should build");
        let module = assembler
            .module(["logger_exit_retry"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .cut (.alt ",
                    "(.read_log >>= (\\_message -> .r ())) ",
                    "(.exit.success))\n",
                ),
            )
            .build()
            .expect("logger retry fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("logger retry fixture should define its effect");
        let fallback = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fallback_values = fallback.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            fallback_values
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .push(diagnostic.message().to_owned());
        });
        let installation = supervisor.install().expect("logger should install");
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::for_runtime(&input.runtime),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let task = EffectRun::new(
            &input.runtime,
            &effect,
            MainEffects::new(assembler.clone()),
            host,
        )
        .schedule_diagnostic_consumer(&installation.lifecycle, &input.diagnostic_ingress)
        .expect("logger root should enter coordinator work");

        input.runtime.pump_until_stable();
        assert!(
            matches!(input.runtime.readiness(), glam::RuntimeReadiness::Ready(_)),
            "the retryable exit should be ready for settlement before disturbance"
        );
        diagnostics.publish_local(Diagnostic::new(
            &input.runtime.values(),
            Severity::Info,
            "arrived before settlement",
        ));

        assert!(matches!(task.run().unwrap(), TaskOutcome::Complete(_)));
        assert!(matches!(
            installation.lifecycle.status(),
            glam::reflection::EffectLifecycleStatus::Complete(_)
        ));
        assert!(
            fallback
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .is_empty(),
            "the disturbed logger must consume the pre-settlement diagnostic"
        );

        diagnostics.publish_local(Diagnostic::new(
            &input.runtime.values(),
            Severity::Info,
            "after terminalization",
        ));
        supervisor
            .deliver_fallback()
            .expect("terminal route should deliver later diagnostics");
        assert_eq!(
            *fallback
                .lock()
                .expect("fallback collection mutex should not be poisoned"),
            ["after terminalization"]
        );
        supervisor.finish(&installation);
    }

    #[test]
    fn recursive_logger_drains_input_queued_before_its_first_poll() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("prequeued logger assembler should build");
        let module = assembler
            .module(["prequeued_logger"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "object logger as logger_object with\n",
                    "  run = (.cut (.alt ",
                    "(.read_log >>= (\\_message -> .r ())) ",
                    "(.exit.success))) =>> logger_object\n",
                    "  eff = logger_object.run.eff\n",
                    "refl.effect = (.heap.get ['start] >>= ",
                    "(\\start -> (start == 1) =>> .r ())) =>> logger\n",
                    "refl.start = .heap.set ['start] 1\n",
                ),
            )
            .build()
            .expect("prequeued logger fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("prequeued logger fixture should define its effect");
        let start = assembler
            .get(module.value(), "refl.start")
            .expect("prequeued logger fixture should define its release effect");
        let fallback = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fallback_messages = fallback.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            fallback_messages
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .push(diagnostic.message().to_owned());
        });
        let installation = supervisor.install().expect("logger should install");
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::for_runtime(&input.runtime),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let task = EffectRun::new(
            &input.runtime,
            &effect,
            MainEffects::new(assembler.clone()),
            host.clone(),
        )
        .schedule_diagnostic_consumer(&installation.lifecycle, &input.diagnostic_ingress)
        .expect("recursive logger root should enter coordinator work");
        input.runtime.pump_until_stable();
        assert!(matches!(
            input.runtime.readiness(),
            RuntimeReadiness::Deadlocked(_)
        ));
        assert!(matches!(
            installation.lifecycle.status(),
            glam::reflection::EffectLifecycleStatus::Blocked
        ));

        diagnostics.publish_local(Diagnostic::new(
            &input.runtime.values(),
            Severity::Info,
            "queued before first poll",
        ));
        let (_generation, _store, input_snapshot) = input.task_capability.transaction_snapshot();
        let mut input_probe = RuntimeEventJournal::new(input_snapshot);
        assert!(
            input_probe
                .read(&input.diagnostic_reader)
                .expect("diagnostic probe should match its runtime")
                .is_some(),
            "the barrier must release only after the diagnostic is admitted"
        );
        let release_lifecycle = EffectLifecycle::new(&input.runtime);
        let release = EffectRun::new(&input.runtime, &start, MainEffects::new(assembler), host)
            .schedule(&release_lifecycle)
            .expect("logger release effect should schedule");
        assert!(matches!(release.run().unwrap(), TaskOutcome::Complete(_)));

        input.runtime.pump_until_stable();
        let RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("the recursive logger should drain prequeued input and vote to exit")
        };
        let report = snapshot.settle().expect("logger exit should settle");
        assert!(report.task_failures().is_empty());
        assert!(report.killed_work().is_empty());
        assert!(
            task.run().is_err(),
            "settlement should terminalize the exit vote"
        );
        supervisor.finish(&installation);
        assert_eq!(
            supervisor
                .fallback_and_deliver()
                .expect("terminal logger should drain fallback input"),
            0
        );
        assert!(
            fallback
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .is_empty()
        );
    }

    #[test]
    fn settled_report_renders_task_and_nonfallback_delivery_failures_once() {
        let source_diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&source_diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("reporting assembler should build");
        let module = assembler
            .module(["settled_failure_report"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .task.new (.fail) >>= (\\_task -> .r ())\n",
                ),
            )
            .build()
            .expect("failure report fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("failure report fixture should define its effect");
        let rendered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let rendered_messages = rendered.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            rendered_messages
                .lock()
                .expect("rendered diagnostic collection should not be poisoned")
                .push(diagnostic);
        });
        supervisor
            .fallback_and_deliver()
            .expect("reporting should begin on fallback");
        let logger_diagnostics = DiagnosticBus::for_runtime(&input.runtime);
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            logger_diagnostics.clone(),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let lifecycle = EffectLifecycle::new(&input.runtime);
        let task = EffectRun::new(
            &input.runtime,
            &effect,
            MainEffects::new(assembler.clone()),
            host,
        )
        .schedule(&lifecycle)
        .expect("failure-report root should schedule");
        assert!(task.run().is_err());

        let failed_output = input
            .runtime
            .output_endpoint(Ok::<Value, Error>, |_value| {
                Err(Error::new("nonfallback output adapter failed"))
            })
            .expect("failed output endpoint should register");
        let (_generation, store, snapshot) = input.task_capability.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events
            .write(&failed_output.writer(), input.runtime.values().integer(1))
            .expect("failed output intent should buffer");
        assert_eq!(
            input
                .task_capability
                .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events,),
            glam::reflection::StoreCommitResult::Committed
        );
        assert!(matches!(
            failed_output.delivery().deliver_next().unwrap(),
            Some(RuntimeDeliveryOutcome::Failed(_))
        ));

        input.runtime.pump_until_stable();
        let glam::RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("terminal failures should be retained state, not active work")
        };
        let mut report = snapshot.settle().expect("failure report should settle");
        assert_eq!(report.task_failures().len(), 1);
        assert_eq!(report.delivery_failures().failures().len(), 1);
        assert_eq!(source_diagnostics.counts().total(), 0);
        assert_eq!(logger_diagnostics.counts().total(), 0);

        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 2);
        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 0);
        let rendered = rendered
            .lock()
            .expect("rendered diagnostic collection should not be poisoned");
        assert_eq!(rendered.len(), 2);
        assert!(
            rendered
                .iter()
                .any(|diagnostic| diagnostic.message().contains("reflection task failed"))
        );
        assert!(rendered.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("nonfallback output adapter failed")
        }));
        assert!(
            rendered
                .iter()
                .all(|diagnostic| { assembler.get(diagnostic.emission(), "msg.context").is_ok() })
        );
        assert_eq!(source_diagnostics.counts().total(), 0);
        assert_eq!(logger_diagnostics.counts().total(), 0);
    }

    #[test]
    fn settled_report_renders_exit_errors_and_killed_work_once() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("settlement-report assembler should build");
        let module = assembler
            .module(["settled_disposition_report"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.exit = .exit.error {msg:{text:\"settled exit failure\"}}\n",
                    "refl.blocked = .cut (.read_log)\n",
                ),
            )
            .build()
            .expect("settlement-report fixture should compile");
        let exit = assembler
            .get(module.value(), "refl.exit")
            .expect("fixture should define its exit effect");
        let blocked = assembler
            .get(module.value(), "refl.blocked")
            .expect("fixture should define its blocked effect");
        let rendered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let rendered_diagnostics = rendered.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            rendered_diagnostics
                .lock()
                .expect("rendered diagnostic collection should not be poisoned")
                .push(diagnostic);
        });
        supervisor
            .fallback_and_deliver()
            .expect("reporting should begin on fallback");
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::for_runtime(&input.runtime),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let exit_lifecycle = EffectLifecycle::new(&input.runtime);
        let blocked_lifecycle = EffectLifecycle::new(&input.runtime);
        let _exit_task = EffectRun::new(
            &input.runtime,
            &exit,
            MainEffects::new(assembler.clone()),
            host.clone(),
        )
        .schedule(&exit_lifecycle)
        .expect("exit effect should schedule");
        let _blocked_task = EffectRun::new(
            &input.runtime,
            &blocked,
            MainEffects::new(assembler.clone()),
            host,
        )
        .schedule(&blocked_lifecycle)
        .expect("blocked effect should schedule");

        input.runtime.pump_until_stable();
        let glam::RuntimeReadiness::Deadlocked(snapshot) = input.runtime.readiness() else {
            panic!("an exit vote plus a blocked reader should retain a deadlock")
        };
        let mut report = snapshot
            .kill(glam::RuntimeKillReason::Deadlock)
            .settle()
            .expect("forced deadlock report should settle");
        assert!(report.dispositions().iter().any(|disposition| {
            matches!(disposition.kind(), RuntimeDispositionKind::ExitError(_))
        }));
        let exit_message = report
            .dispositions()
            .iter()
            .find_map(|disposition| match disposition.kind() {
                RuntimeDispositionKind::ExitError(message) => Some(message),
                _ => None,
            })
            .expect("report should retain the exit message");
        assert_eq!(
            assembler
                .get_evaluated(exit_message, "msg.text")
                .expect("exit error should retain its structured message")
                .as_bytes(),
            Some(b"settled exit failure".as_slice())
        );
        assert_eq!(report.killed_work().len(), 1);

        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 2);
        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 0);
        let rendered = rendered
            .lock()
            .expect("rendered diagnostic collection should not be poisoned");
        assert_eq!(rendered.len(), 2);
        assert!(
            rendered
                .iter()
                .any(|diagnostic| diagnostic.message() == "settled exit failure"),
            "rendered messages: {:?}",
            rendered.iter().map(Diagnostic::message).collect::<Vec<_>>()
        );
        assert!(rendered.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("runtime killed reflection_task work")
        }));
    }

    #[test]
    fn settled_report_rendering_drains_work_admitted_by_fallback_delivery() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("reentrant reporting assembler should build");
        let module = assembler
            .module(["reentrant_settled_report"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .task.new (.fail) >>= (\\_task -> .r ())\n",
                ),
            )
            .build()
            .expect("reentrant reporting fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("fixture should define its effect");
        let rendered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let rendered_messages = rendered.clone();
        let republished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_republished = republished.clone();
        let callback_bus = diagnostics.clone();
        let callback_values = input.runtime.values();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            rendered_messages
                .lock()
                .expect("rendered diagnostic collection should not be poisoned")
                .push(diagnostic.message().to_owned());
            if !callback_republished.swap(true, std::sync::atomic::Ordering::SeqCst) {
                callback_bus.publish_local(Diagnostic::new(
                    &callback_values,
                    Severity::Info,
                    "admitted during fallback delivery",
                ));
            }
        });
        supervisor
            .fallback_and_deliver()
            .expect("reporting should begin on fallback");
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::for_runtime(&input.runtime),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let lifecycle = EffectLifecycle::new(&input.runtime);
        let task = EffectRun::new(
            &input.runtime,
            &effect,
            MainEffects::new(assembler.clone()),
            host,
        )
        .schedule(&lifecycle)
        .expect("reentrant reporting root should schedule");
        assert!(task.run().is_err());
        input.runtime.pump_until_stable();
        let glam::RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("retained task failure should be ready")
        };
        let mut report = snapshot
            .settle()
            .expect("task failure report should settle");

        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 1);
        assert_eq!(
            *rendered
                .lock()
                .expect("rendered diagnostic collection should not be poisoned"),
            [
                "reflection task failed permanently".to_owned(),
                "admitted during fallback delivery".to_owned(),
            ]
        );
        assert_eq!(diagnostics.counts().total(), 1);
    }

    #[test]
    fn fallback_delivery_failure_is_retained_without_recursive_rendering() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_calls = fallback_calls.clone();
        let supervisor = LoggerSupervisor::new_fallible(input.clone(), move |_diagnostic| {
            callback_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(Error::new("fallback output adapter failed"))
        });
        supervisor
            .fallback_and_deliver()
            .expect("reporting should begin on fallback");

        let failed_output = input
            .runtime
            .output_endpoint(Ok::<Value, Error>, |_value| {
                Err(Error::new("initial output adapter failed"))
            })
            .expect("initial failed output endpoint should register");
        let (_generation, store, snapshot) = input.task_capability.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events
            .write(&failed_output.writer(), input.runtime.values().integer(1))
            .expect("initial failed output intent should buffer");
        assert_eq!(
            input
                .task_capability
                .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events),
            glam::reflection::StoreCommitResult::Committed
        );
        assert!(matches!(
            failed_output.delivery().deliver_next().unwrap(),
            Some(RuntimeDeliveryOutcome::Failed(_))
        ));
        let glam::RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("a retained delivery failure should be ready")
        };
        let mut report = snapshot
            .settle()
            .expect("initial delivery failure should settle");

        let error = supervisor
            .render_settled_report(&mut report)
            .expect_err("fallback delivery should report its adapter failure");
        assert_eq!(error.to_string(), "fallback output adapter failed");
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            supervisor.render_settled_report(&mut report).unwrap(),
            0,
            "accepted fallback output must not be replayed after adapter failure"
        );
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let glam::RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("failed fallback delivery should remain reportable state")
        };
        let mut repeated = snapshot
            .settle()
            .expect("fallback delivery failure should settle");
        assert_eq!(repeated.delivery_failures().failures().len(), 2);
        assert_eq!(supervisor.render_settled_report(&mut repeated).unwrap(), 0);
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            repeated
                .delivery_failures()
                .failures()
                .iter()
                .any(|failure| failure.endpoint_id() == supervisor.fallback_delivery.id())
        );
    }

    #[test]
    fn batch_settlement_remains_failed_when_fallback_rendering_fails() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let supervisor = LoggerSupervisor::new_fallible(input.clone(), |_diagnostic| {
            Err(Error::new("fallback renderer failed"))
        });
        supervisor
            .fallback_and_deliver()
            .expect("empty fallback route should activate");

        let failed_output = input
            .runtime
            .output_endpoint(Ok::<Value, Error>, |_value| {
                Err(Error::new("authoritative adapter failure"))
            })
            .expect("failed output endpoint should register");
        let (_generation, store, snapshot) = input.task_capability.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events
            .write(&failed_output.writer(), input.runtime.values().integer(1))
            .expect("failed output intent should buffer");
        assert_eq!(
            input
                .task_capability
                .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events),
            glam::reflection::StoreCommitResult::Committed
        );
        assert!(matches!(
            failed_output.delivery().deliver_next().unwrap(),
            Some(RuntimeDeliveryOutcome::Failed(_))
        ));

        assert!(settle_batch_runtime(&input.runtime, &supervisor));
        let failures = input.runtime.delivery_failure_snapshot().failures();
        assert_eq!(failures.len(), 2);
        assert!(failures.iter().any(|failure| {
            failure
                .error()
                .to_string()
                .contains("fallback renderer failed")
        }));
    }

    #[test]
    fn diagnostic_publication_racing_ingress_rearm_is_routed_once() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let supervisor = LoggerSupervisor::new(input.clone(), |_| {});
        let first = supervisor.install().expect("first logger should install");
        let values = input.runtime.values();
        let publishing_bus = diagnostics.clone();
        let publishing_values = values.clone();
        let (ready, wait) = std::sync::mpsc::channel();
        let (resume, resumed) = std::sync::mpsc::channel();
        let publisher = thread::spawn(move || {
            ready.send(()).expect("rearm test should signal readiness");
            resumed
                .recv()
                .expect("rearm test should resume publication");
            publishing_bus.publish_local(Diagnostic::new(
                &publishing_values,
                Severity::Warning,
                "publication during rearm",
            ))
        });
        wait.recv()
            .expect("publisher should reach the rearm barrier");
        supervisor.finish(&first);
        let _second = supervisor.install().expect("logger should rearm");
        input
            .diagnostic_ingress
            .activate()
            .expect("ingress should rearm");
        resume.send(()).expect("publisher should resume");
        let event = publisher.join().expect("publisher should not panic");

        assert_eq!(event.sequence(), diagnostics.counts().latest_sequence());
        assert_eq!(diagnostics.counts().total(), 1);
        assert_eq!(
            input
                .take_diagnostic()
                .expect("racing publication should remain buffered")
                .message(),
            "publication during rearm"
        );
        assert!(input.take_diagnostic().is_none());
    }

    #[test]
    fn logger_supervisor_teardown_does_not_retain_runtime_resources() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let weak_capability = Arc::downgrade(&input.task_capability);
        let weak_input = Arc::downgrade(&input);
        let supervisor = LoggerSupervisor::new(input.clone(), |_| {});
        let installation = supervisor.install().expect("logger should install");
        drop(input);
        drop(installation);
        drop(supervisor);
        drop(diagnostics);

        assert!(weak_input.upgrade().is_none());
        assert!(weak_capability.upgrade().is_none());
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
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("logger assembler should build");
        let host = LoggerTaskHost::new(
            input.clone(),
            diagnostics.clone(),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        );

        <LoggerTaskHost as ReflectionServices>::emit_diagnostic(
            &host,
            Diagnostic::new(&input.runtime.values(), Severity::Error, "session output"),
        );

        let (_generation, _store, snapshot) = input.task_capability.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
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
