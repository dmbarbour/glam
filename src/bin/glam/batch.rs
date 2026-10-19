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
    Diagnostic, DiagnosticBus, Error, EvaluationRuntime, FileSourceSystem, Machine, ModuleInput,
    QuiescenceReport, Runtime, RuntimeDispositionKind, RuntimeKillReason, RuntimeReadiness,
    Severity, Values,
};

use crate::DiagnosticBusLocal;
use crate::configuration::logger::{
    LOGGER, SettledReportSelection, drain_fallback, render_settled_report, render_unread,
    settled_report_diagnostics, start_logger,
};
use crate::configuration::{CLI, InitialCliEnvironment, PreparationFailure, PreparedAssembly};
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
            mut runtime,
            error,
        } = *failure;
        let assembler = glam::transition::assembler(&runtime);
        let diagnostics = assembler.diagnostic_bus();
        publish_error(&diagnostics, &assembler.values(), &error);
        finish_local_files(
            &local_files,
            failure_manifest,
            &diagnostics,
            &assembler.values(),
        );
        render_unread_diagnostics(&mut runtime);
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
            &prepared.assembler().diagnostic_bus(),
            &prepared.assembler().values(),
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
            &prepared.assembler().diagnostic_bus(),
            &prepared.assembler().values(),
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
        mut runtime,
        ..
    } = prepared;
    let assembler = glam::transition::assembler(&runtime).clone();
    let assembler_diagnostics = assembler.diagnostic_bus();
    let values = assembler.values();
    // The batch is one turn on the root, which starts the logger, assembles
    // on the `cli` machine, and joins the logger before the turn ends.
    let (operation_failed, logger_diagnostics) = runtime
        .run((), |root| {
            let mut logger = start_logger(root, &assembler)?;
            let result = root.run(CLI, |m| assemble(m, inputs, &assembly_args));
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
            let renderer = logger.renderer.clone();
            let forked = logger.thread.is_some();
            operation_failed |= super::profile::time("settle", || {
                settle_batch_runtime(
                    &assembler.evaluation_runtime(),
                    &values,
                    |diagnostic| renderer.emit(&diagnostic),
                    || {
                        // A configured logger's fork holds the fallback, which
                        // receives nothing until the configured logger ends.
                        if forked {
                            return Ok(());
                        }
                        root.run(LOGGER, |logger| drain_fallback(logger, &renderer))
                    },
                )
            });

            if let Some(thread) = logger.thread.take()
                && let Err(error) = root.join(thread)
            {
                operation_failed = true;
                eprintln!("error: the logger ended in error: {error}");
            }
            if let Err(error) = root.recover(LOGGER, |logger| render_unread(logger, &renderer)) {
                operation_failed = true;
                eprintln!("error: could not render fallback diagnostics: {error}");
            }
            Ok((operation_failed, logger.diagnostics.clone()))
        })
        .expect("the batch's turn returns its failures as diagnostics");
    super::profile::report(&runtime);

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
    values: &Values,
    mut render: impl FnMut(Diagnostic),
    mut drain_fallback: impl FnMut() -> Result<(), Error>,
) -> bool {
    let mut failed = false;

    loop {
        runtime.pump_until_stable();
        if let Err(error) = drain_fallback() {
            failed = true;
            eprintln!("error: could not drain fallback diagnostics: {error}");
        }
        let snapshot = match runtime.readiness() {
            RuntimeReadiness::Busy => continue,
            RuntimeReadiness::MaintenanceRequired(maintenance) => {
                if let Err(error) = maintenance.service()
                    && error.kind() != glam::RuntimeMaintenanceErrorKind::RuntimeChanged
                {
                    failed = true;
                }
                continue;
            }
            RuntimeReadiness::MaintenanceFailed(maintenance) => {
                if let Some(failure) = maintenance.failure() {
                    eprintln!("error: managed maintenance failed: {}", failure.message());
                } else {
                    eprintln!(
                        "error: evaluation runtime {} managed heap is permanently poisoned",
                        maintenance.runtime_id().get()
                    );
                }
                return true;
            }
            RuntimeReadiness::Poisoned => {
                eprintln!(
                    "error: evaluation runtime {} is poisoned by an internal panic",
                    runtime.id().get()
                );
                return true;
            }
            RuntimeReadiness::Ready(snapshot) => snapshot,
            RuntimeReadiness::Deadlocked(deadlock) => deadlock.kill(RuntimeKillReason::Deadlock),
        };
        let mut report = match snapshot.settle() {
            Ok(report) => report,
            Err(_) => continue,
        };

        failed |= settled_report_is_fatal(&report);
        match render_settled_report(&mut report, values, &mut render) {
            Ok(0) => return failed,
            Ok(_) => {}
            Err(error) => {
                eprintln!("error: could not render runtime settlement: {error}");
                return true;
            }
        }
    }
}

fn settled_report_is_fatal(report: &QuiescenceReport) -> bool {
    !report.task_failures().is_empty()
        || !report.delivery_failures().failures().is_empty()
        || !report.maintenance_failures().is_empty()
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
            RuntimeReadiness::MaintenanceRequired(maintenance) => {
                if let Err(error) = maintenance.service()
                    && error.kind() != glam::RuntimeMaintenanceErrorKind::RuntimeChanged
                {
                    failed = true;
                }
                continue;
            }
            RuntimeReadiness::MaintenanceFailed(maintenance) => {
                if let Some(failure) = maintenance.failure() {
                    eprintln!("error: managed maintenance failed: {}", failure.message());
                } else {
                    eprintln!(
                        "error: evaluation runtime {} managed heap is permanently poisoned",
                        maintenance.runtime_id().get()
                    );
                }
                return true;
            }
            RuntimeReadiness::Poisoned => {
                eprintln!(
                    "error: evaluation runtime {} is poisoned by an internal panic",
                    runtime.id().get()
                );
                return true;
            }
            RuntimeReadiness::Ready(snapshot) => snapshot,
            RuntimeReadiness::Deadlocked(deadlock) => deadlock.kill(RuntimeKillReason::Deadlock),
        };
        let mut report = match snapshot.settle() {
            Ok(report) => report,
            Err(_) => continue,
        };
        failed |= settled_report_is_fatal(&report);
        let selection = SettledReportSelection::pending(&report);
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
    mut prepared: PreparedAssembly,
    manifest: Option<&Path>,
    operation_failed: bool,
) -> ExitCode {
    let diagnostics = prepared.assembler().diagnostic_bus();
    let runtime_failed = settle_batch_runtime_default(
        &prepared.assembler().evaluation_runtime(),
        &diagnostics,
        &prepared.assembler().values(),
    );
    let file_failure = finish_local_files(
        &prepared.local_files,
        manifest,
        &diagnostics,
        &prepared.assembler().values(),
    );
    let render_failure = render_unread_diagnostics(&mut prepared.runtime);
    if operation_failed
        || runtime_failed
        || file_failure
        || render_failure
        || diagnostics.counts().errors() != 0
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Renders every diagnostic waiting at the logger, where the CLI ends
/// without a turn that started one, and returns whether that failed.
fn render_unread_diagnostics(runtime: &mut Runtime) -> bool {
    let renderer = DefaultLogger::new(glam::transition::assembler(runtime).clone());
    match runtime.recover(LOGGER, |logger| render_unread(logger, &renderer)) {
        Ok(()) => false,
        Err(error) => {
            eprintln!("error: could not render diagnostics: {error}");
            true
        }
    }
}

pub(super) fn configured_cli(arguments: CliArguments, inspection: Option<bool>) -> ExitCode {
    let mut prepared = match prepare_assembly(arguments.clone(), None, None) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let configuration = prepared.configuration();
    let expansion = match expand_configured(prepared.assembler(), &configuration, arguments) {
        Ok(expansion) => expansion,
        Err(error) => {
            prepared.fail_environment("configured CLI expansion failed");
            for diagnostic in error.diagnostics() {
                prepared
                    .assembler()
                    .diagnostic_bus()
                    .publish_local(diagnostic.clone());
            }
            prepared.assembler().diagnostic_bus().publish_local(
                error
                    .diagnostic(&prepared.assembler().values())
                    .expect("configured CLI diagnostics belong to its assembler runtime"),
            );
            return finish_without_logger(prepared, None, true);
        }
    };
    let (command, diagnostics) = expansion.into_parts();
    for diagnostic in diagnostics {
        prepared
            .assembler()
            .diagnostic_bus()
            .publish_local(diagnostic);
    }
    if let Some(nul_terminated) = inspection {
        let parts = command.into_parts();
        if let Err(error) = prepared.resolve_environment(&parts) {
            publish_error(
                &prepared.assembler().diagnostic_bus(),
                &prepared.assembler().values(),
                &error,
            );
            return finish_without_logger(prepared, None, true);
        }
        let output = format_configured_arguments(&parts.process_args, nul_terminated);
        let failed = if let Err(error) = io::stdout().write_all(&output) {
            prepared
                .assembler()
                .diagnostic_bus()
                .publish_local(Diagnostic::new(
                    &prepared.assembler().values(),
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

/// Compiles the assembly on the `cli` machine, over the prelude's initial
/// definitions, and evaluates its result to bytes.
fn assemble(
    m: &mut Machine<'_, '_>,
    inputs: Vec<ModuleInput>,
    cli_args: &[std::ffi::OsString],
) -> Result<Bytes, Error> {
    m.get("prelude")?;
    m.access("assembly_definitions")?;
    for argument in cli_args {
        m.push_bytes(argument.as_encoded_bytes().to_vec());
    }
    m.make_list(cli_args.len())?;
    m.get("environment")?;
    m.apply(2)?;
    super::profile::time("build", || m.compile_over(["assembly"], inputs))?;
    m.stash(1)?;
    m.get("prelude")?;
    m.access("assembly_result")?;
    m.stash_pop(1)?;
    m.apply(1)?;
    super::profile::time("evaluate", || m.pop_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_batch_settlement_services_managed_maintenance() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let diagnostics = DiagnosticBus::for_runtime(&runtime);
        let values = runtime.values();
        runtime
            .request_managed_collection()
            .expect("explicit maintenance should publish");

        assert!(!settle_batch_runtime_default(
            &runtime,
            &diagnostics,
            &values
        ));
        assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));
    }
}
