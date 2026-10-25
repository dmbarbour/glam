//! The reports a settled runtime holds, rendered as diagnostics.

use std::sync::Arc;

use glam::{
    Diagnostic, Error, HandleValue, QuiescenceReport, RuntimeDeadlockWork, RuntimeDeliveryFailure,
    RuntimeDependency, RuntimeDisposition, RuntimeDispositionKind, RuntimeMaintenanceFailure,
    RuntimeWorkKind, RuntimeWorkState, Severity, Values,
};

/// The reports of a settlement that are still to be rendered.
pub(crate) struct SettledReportSelection {
    pub(crate) task_failures: Vec<glam::ReasoningFailure>,
    pub(crate) failed_machines: Vec<glam::FailedMachine>,
    pub(crate) delivery_failures: Vec<Arc<RuntimeDeliveryFailure>>,
    pub(crate) exit_errors: Vec<RuntimeDisposition>,
    pub(crate) killed_work: Vec<RuntimeDeadlockWork>,
    pub(crate) maintenance_failures: Vec<RuntimeMaintenanceFailure>,
}

impl SettledReportSelection {
    /// The reports of `report` still to be rendered.
    pub(crate) fn pending(report: &QuiescenceReport) -> Self {
        Self {
            task_failures: report.pending_task_failure_reports().to_vec(),
            failed_machines: report.pending_failed_machine_reports().to_vec(),
            delivery_failures: report
                .pending_delivery_failure_reports()
                .failures()
                .into_iter()
                .collect(),
            exit_errors: report.pending_exit_error_reports().to_vec(),
            killed_work: report.pending_killed_work_reports().to_vec(),
            maintenance_failures: report.pending_maintenance_failure_reports().to_vec(),
        }
    }
}

/// Renders the reports of `report` still to be rendered, each once, and
/// returns how many it rendered. They count as rendered before `render`
/// runs, so that a rendering that fails partway never repeats one.
pub(crate) fn render_settled_report(
    report: &mut QuiescenceReport,
    values: &Values,
    mut render: impl FnMut(Diagnostic),
) -> Result<usize, Error> {
    let diagnostics = settled_report_diagnostics(values, SettledReportSelection::pending(report))?;
    report.mark_reports_enqueued();
    let rendered = diagnostics.len();
    for diagnostic in diagnostics {
        render(diagnostic);
    }
    Ok(rendered)
}

pub(crate) fn settled_report_diagnostics(
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
    for machine in selection.failed_machines {
        let path = values.list(machine.path().keys().iter().map(|key| values.key(key)))?;
        let context = runtime_report_context(values, "machine_failure", vec![("machine", path)])?;
        diagnostics.push(machine.diagnostic().clone().with_context(values, context)?);
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
    for failure in selection.maintenance_failures {
        diagnostics.push(maintenance_failure_diagnostic(
            values,
            failure.id(),
            failure.kind(),
            failure.message(),
        )?);
    }
    for disposition in selection.exit_errors {
        let RuntimeDispositionKind::ExitError(message) = disposition.kind() else {
            unreachable!("report selection retains only error exits")
        };
        let mut args = vec![("work", report_id(values, disposition.work_id())?)];
        if let Some(session) = disposition.session_id() {
            args.push(("session", report_id(values, session)?));
        }
        if let Some(task) = disposition.task_id() {
            args.push(("task", report_id(values, task)?));
        }
        let context = runtime_report_context(values, "exit_error", args)?;
        diagnostics.push(
            Diagnostic::from_emission(values, Severity::Error, message.clone())?
                .with_context(values, context)?,
        );
    }
    for work in selection.killed_work {
        let blocked_error = work.blocked_diagnostic().cloned();
        let mut args = vec![
            ("work", report_id(values, work.work_id())?),
            (
                "kind",
                values.atom_from_text(runtime_work_kind_name(work.kind())),
            ),
            (
                "state",
                values.atom_from_text(runtime_work_state_name(work.state())),
            ),
        ];
        if let Some(session) = work.session_id() {
            args.push(("session", report_id(values, session)?));
        }
        if let Some(lazy) = work.lazy_id() {
            args.push(("lazy", report_id(values, lazy)?));
        }
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
        // The retained error stays structured: it follows the kill frame as a
        // nested message with its own emission and context, so the headline
        // only notes that it exists.
        if blocked_error.is_some() {
            message.push_str(", with a retained error");
        }
        let mut diagnostic = Diagnostic::new(values, Severity::Error, message);
        if let Some(blocked) = blocked_error {
            diagnostic = diagnostic.with_context(values, blocked.emission().clone())?;
        }
        diagnostics.push(diagnostic.with_context(values, context)?);
    }
    Ok(diagnostics)
}

fn maintenance_failure_diagnostic(
    values: &Values,
    id: u64,
    kind: glam::RuntimeMaintenanceFailureKind,
    message: &str,
) -> Result<Diagnostic, Error> {
    let context = runtime_report_context(
        values,
        "maintenance_failure",
        vec![
            ("failure", report_id(values, id)?),
            (
                "kind",
                values.atom_from_text(match kind {
                    glam::RuntimeMaintenanceFailureKind::CollectorPanic => "collector_panic",
                    glam::RuntimeMaintenanceFailureKind::FinalizerPanic => "finalizer_panic",
                    glam::RuntimeMaintenanceFailureKind::Poisoned => "poisoned",
                }),
            ),
        ],
    )?;
    Diagnostic::new(values, Severity::Error, message).with_context(values, context)
}

fn report_id(values: &Values, id: u64) -> Result<HandleValue, Error> {
    values.number_from_text(id.to_string())
}

fn runtime_report_context(
    values: &Values,
    operation: &str,
    args: Vec<(&str, HandleValue)>,
) -> Result<HandleValue, Error> {
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
        RuntimeWorkKind::LazyRoute => "lazy_route",
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
) -> Result<HandleValue, Error> {
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
        RuntimeDependency::LazyWait { wait_id, lazy_id } => values.record([(
            "lazy_wait",
            values.record([
                ("wait", report_id(values, *wait_id)?),
                ("lazy", report_id(values, *lazy_id)?),
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

#[cfg(test)]
mod tests {
    use glam::Severity;

    use super::{SettledReportSelection, maintenance_failure_diagnostic};

    fn assert_settled_report_owner_inventory(selection: &SettledReportSelection) {
        let SettledReportSelection {
            task_failures: _,
            failed_machines: _,
            delivery_failures: _,
            exit_errors: _,
            killed_work: _,
            maintenance_failures: _,
        } = selection;
    }

    #[test]
    fn settled_report_owner_inventory_is_compile_exhaustive() {
        let _: fn(&SettledReportSelection) = assert_settled_report_owner_inventory;
    }

    #[test]
    fn maintenance_failures_render_as_structured_error_diagnostics() {
        let runtime = glam::EvaluationRuntime::new(0).unwrap();
        let values = runtime.values();
        let diagnostic = maintenance_failure_diagnostic(
            &values,
            7,
            glam::RuntimeMaintenanceFailureKind::FinalizerPanic,
            "injected finalizer panic",
        )
        .unwrap();

        assert_eq!(diagnostic.severity(), Severity::Error);
        assert_eq!(diagnostic.message(), "injected finalizer panic");
        assert!(
            values
                .access_names(diagnostic.emission(), ["msg", "context"])
                .is_ok(),
            "maintenance diagnostics should retain structured runtime context"
        );
    }
}
