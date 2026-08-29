use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use crate::command_line::{
    CliArguments, CommandPlan, CommandPlanParts, expand_configured, format_configured_arguments,
    parse_worker_count,
};
use bytes::Bytes;
use glam::{
    Assembler, Diagnostic, DiagnosticBus, Error, EvaluationRuntime, FileSourceSystem, ModuleInput,
    QuiescenceReport, RuntimeDispositionKind, RuntimeKillReason, RuntimeReadiness, Severity, Value,
    Values,
};

use crate::DiagnosticBusLocal;
use crate::configuration::logger::{
    LoggerRun, LoggerSupervisor, SettledReportSelection, settled_report_diagnostics, start_logger,
};
use crate::configuration::{InitialCliEnvironment, PreparationFailure, PreparedAssembly};
use crate::rendering::DefaultLogger;

pub(super) fn finish_local_files(
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

pub(super) fn prepare_assembly(
    cli_arguments: CliArguments,
    failure_manifest: Option<&Path>,
    initial_environment: Option<InitialCliEnvironment>,
) -> Result<PreparedAssembly, ExitCode> {
    crate::configuration::prepare(cli_arguments, initial_environment).map_err(|failure| {
        let PreparationFailure {
            local_files,
            log_host,
            assembler,
            error,
        } = *failure;
        let diagnostics = assembler.diagnostic_bus();
        publish_error(&diagnostics, &assembler.values(), &error);
        finish_local_files(
            &local_files,
            failure_manifest,
            &diagnostics,
            &assembler.values(),
        );
        log_host.drain_default(&DefaultLogger::new(assembler));
        ExitCode::from(1)
    })
}

pub(super) fn assemble_inputs(command: CommandPlan) -> ExitCode {
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
pub(super) fn settle_batch_runtime(
    runtime: &EvaluationRuntime,
    supervisor: &LoggerSupervisor,
) -> bool {
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

pub(super) fn finish_without_logger(
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

pub(super) fn configured_cli(arguments: CliArguments, inspection: Option<bool>) -> ExitCode {
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
                .as_bytes(&values)?
                .ok_or_else(|| Error::new("asm.result did not evaluate to binary data"))
        })
        .map_err(|error| {
            error
                .with_context(&values, context)
                .expect("assembly-result context belongs to the assembler runtime")
        })
}

pub(super) fn assembly_result_context(values: &Values) -> Result<Value, Error> {
    values.record([(
        "asm",
        values.record([("result", values.text("asm.result"))])?,
    )])
}
