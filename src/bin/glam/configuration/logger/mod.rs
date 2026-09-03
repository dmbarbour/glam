use std::sync::Arc;
use std::thread;

use glam::reflection::{EffectRun, TaskOutcome};
use glam::{Assembler, Diagnostic, DiagnosticBus, Severity, Value};

use crate::DiagnosticBusLocal;
use crate::configuration::{entry_context, with_path_lookup_context};
use crate::rendering::DefaultLogger;

mod effects;
mod supervisor;

pub(crate) use effects::{LoggerTaskHost, MainEffects};
pub(crate) use supervisor::{
    LogHost, LoggerSupervisor, SettledReportSelection, settled_report_diagnostics,
};

pub(crate) fn start_logger(
    assembler: &Assembler,
    configuration: &Value,
    input: Arc<LogHost>,
) -> LoggerRun {
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
        Ok(logger)
            if logger
                .array_items()
                .is_ok_and(|items| items.is_some_and(|items| items.is_empty())) =>
        {
            None
        }
        Ok(logger) => Some(logger.into_value()),
        Err(error) => {
            let diagnostic = error
                .with_context(
                    &values,
                    entry_context(&values, "log").expect("configuration context is local"),
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
                task_diagnostics.publish_local(
                    error
                        .diagnostic(&task_values)
                        .expect("logger failures belong to the logger runtime"),
                );
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
            entry_context(&task_values, "log").expect("configuration context is local"),
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
                            &task_values,
                            entry_context(&task_values, "log")
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
                                entry_context(&task_values, "log")
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

pub(crate) struct LoggerRun {
    pub(crate) thread: thread::JoinHandle<()>,
    pub(crate) diagnostics: DiagnosticBus,
    pub(crate) supervisor: Arc<LoggerSupervisor>,
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    fn assert_logger_run_owner_inventory(run: &LoggerRun) {
        let LoggerRun {
            thread: _,
            diagnostics: _,
            supervisor: _,
        } = run;
    }

    #[test]
    fn logger_run_owner_inventory_is_compile_exhaustive() {
        let _: fn(&LoggerRun) = assert_logger_run_owner_inventory;
    }
}
