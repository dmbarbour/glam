use std::sync::Arc;
use std::thread;

use glam::reflection::{EffectRun, TaskOutcome};
use glam::{Assembler, Diagnostic, DiagnosticBus, Error, Fork, Machine, Severity, Value};

use crate::DiagnosticBusLocal;
use crate::configuration::{entry_context, select_entry, selects_nothing};
use crate::rendering::DefaultLogger;

mod effects;
mod supervisor;

pub(crate) use effects::{LoggerTaskHost, MainEffects};
pub(crate) use supervisor::{
    LogHost, LoggerSupervisor, SettledReportSelection, settled_report_diagnostics,
};

/// Starts the logger, which the `cli` machine `m` selects, on a fork of
/// its child `logger`.
pub(crate) fn start_logger<'scope>(
    m: &mut Machine<'scope, '_>,
    assembler: &Assembler,
    input: Arc<LogHost>,
) -> LoggerRun<'scope> {
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
    ));
    let values = assembler.values();
    let custom = match select_logger(m) {
        Ok(custom) => custom,
        Err(error) => {
            let diagnostic = error
                .diagnostic(&values)
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
            MainEffects::new(values.clone()),
            host.clone(),
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
    let builder = thread::Builder::new()
        .name("glam-logger".to_owned())
        // The logger evaluates ordinary Glam configuration and therefore
        // needs the same practical stack headroom as the process main thread.
        // Deep evaluator paths are being migrated to explicit machines, but
        // the bootstrap must not make a configured logger depend on the
        // platform's smaller default child-thread stack in the meantime.
        .stack_size(8 * 1024 * 1024);
    let thread = m
        .fork_with(builder, "logger", move |_| {
            let _subscription = subscription;
            if let Some((installation, task)) = scheduled {
                // This client thread delivers what the logger task commits,
                // wherever the task runs, so rendering stays off the workers.
                let outcome = task.run_with(|| host.deliver());
                match outcome {
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
                // The logger's end moved what it left unread to the fallback.
                let _ = task_supervisor.deliver_fallback();
            } else {
                let _ = task_supervisor.fallback_and_deliver();
            }
            Ok(())
        })
        .expect("logger thread should start");
    LoggerRun {
        thread,
        diagnostics,
        supervisor,
    }
}

/// The logger `conf.log` selects, evaluated, or none.
fn select_logger(m: &mut Machine<'_, '_>) -> Result<Option<Value>, Error> {
    m.make_list(0)?;
    m.get("configuration")?;
    m.access("conf.log")?;
    select_entry(m, "log", "conf.log")?;
    m.eval()?;
    if selects_nothing(m)? {
        m.discard(1)?;
        return Ok(None);
    }
    glam::transition::pop_value(m).map(Some)
}

pub(crate) struct LoggerRun<'scope> {
    pub(crate) thread: Fork<'scope>,
    pub(crate) diagnostics: DiagnosticBus,
    pub(crate) supervisor: Arc<LoggerSupervisor>,
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    fn assert_logger_run_owner_inventory(run: &LoggerRun<'_>) {
        let LoggerRun {
            thread: _,
            diagnostics: _,
            supervisor: _,
        } = run;
    }

    #[test]
    fn logger_run_owner_inventory_is_compile_exhaustive() {
        let _: fn(&LoggerRun<'_>) = assert_logger_run_owner_inventory;
    }
}
