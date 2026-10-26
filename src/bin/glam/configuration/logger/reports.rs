//! The issues an analysis of the runtime finds, rendered as diagnostics
//! (`quiescence-is-analyzed`): each with a report context, and each once,
//! since what to do with an analysis is the batch's.

use std::collections::BTreeSet;

use glam::{
    Diagnostic, Error, HandleValue, Key, RuntimeAnalysis, RuntimeDependency, RuntimeIssue,
    RuntimeWaitFamily, RuntimeWork, RuntimeWorkKind, RuntimeWorkState, Severity, Values,
};

/// What names an issue, so a batch publishes each once.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum IssueKey {
    Wait(u64),
    Backlog(Vec<Key>, Option<Key>),
    Obligation(u64, Vec<Key>),
    Machine(Vec<Key>),
    Delivery(u64),
    Maintenance(u64),
}

/// The issues a batch has published.
#[derive(Default)]
pub(crate) struct PublishedIssues(BTreeSet<IssueKey>);

impl PublishedIssues {
    /// The diagnostics of `analysis`'s issues that are not published yet,
    /// which count as published now.
    pub(crate) fn take_new(
        &mut self,
        values: &Values,
        analysis: &RuntimeAnalysis,
    ) -> Result<Vec<Diagnostic>, Error> {
        let mut diagnostics = Vec::new();
        for issue in analysis.issues() {
            match issue {
                RuntimeIssue::Obligation(obligation) => {
                    for (machine, owed) in obligation.acceptors() {
                        let key =
                            IssueKey::Obligation(obligation.promise_id(), machine.keys().to_vec());
                        if self.0.insert(key) {
                            let context = runtime_report_context(
                                values,
                                "pending_obligation",
                                vec![
                                    ("promise", report_id(values, obligation.promise_id())?),
                                    ("machine", machine_path(values, machine.keys())?),
                                ],
                            )?;
                            diagnostics.push(owed.clone().with_context(values, context)?);
                        }
                    }
                }
                issue => {
                    if self.0.insert(issue_key(&issue)) {
                        diagnostics.push(issue_diagnostic(values, &issue)?);
                    }
                }
            }
        }
        Ok(diagnostics)
    }
}

fn issue_key(issue: &RuntimeIssue<'_>) -> IssueKey {
    match issue {
        RuntimeIssue::Wait(wait) => IssueKey::Wait(wait.work().work_id()),
        RuntimeIssue::Backlog(backlog) => {
            IssueKey::Backlog(backlog.machine().keys().to_vec(), backlog.port().cloned())
        }
        RuntimeIssue::Machine(machine) => IssueKey::Machine(machine.path().keys().to_vec()),
        RuntimeIssue::Delivery(failure) => IssueKey::Delivery(failure.delivery_id().get()),
        RuntimeIssue::Maintenance(failure) => IssueKey::Maintenance(failure.id()),
        RuntimeIssue::Obligation(_) => unreachable!("each acceptor of an obligation has its own"),
    }
}

fn issue_diagnostic(values: &Values, issue: &RuntimeIssue<'_>) -> Result<Diagnostic, Error> {
    match issue {
        RuntimeIssue::Wait(wait) => {
            let work = wait.work();
            let erred = wait.family() == RuntimeWaitFamily::ErredWait;
            let mut args = work_args(values, work)?;
            if let Some(cause) = wait.cause_work_id() {
                args.push(("cause", report_id(values, cause)?));
            }
            let message = format!(
                "{} work {} {}",
                runtime_work_kind_name(work.kind()),
                work.work_id(),
                if erred {
                    "waits for a change after an error"
                } else {
                    "waits on a value that nothing in progress will produce"
                }
            );
            let context = runtime_report_context(
                values,
                if erred { "erred_wait" } else { "promise_wait" },
                args,
            )?;
            let mut diagnostic = Diagnostic::new(values, Severity::Error, message);
            // The retained error stays structured: it follows the wait's
            // frame as a nested message with its own emission and context.
            if let Some(blocked) = work.blocked_diagnostic() {
                diagnostic = diagnostic.with_context(values, blocked.emission().clone())?;
            }
            diagnostic.with_context(values, context)
        }
        RuntimeIssue::Backlog(backlog) => {
            let mut args = vec![
                ("machine", machine_path(values, backlog.machine().keys())?),
                ("count", report_id(values, backlog.count() as u64)?),
            ];
            if let Some(port) = backlog.port() {
                args.push(("port", values.key(port)));
            }
            let message = match backlog.port() {
                Some(_) => format!("{} input items left unread", backlog.count()),
                None => format!("{} queued tasks never started", backlog.count()),
            };
            let context = runtime_report_context(values, "backlog", args)?;
            Diagnostic::new(values, Severity::Error, message).with_context(values, context)
        }
        RuntimeIssue::Machine(machine) => {
            let context = runtime_report_context(
                values,
                "machine_failure",
                vec![("machine", machine_path(values, machine.path().keys())?)],
            )?;
            machine.diagnostic().clone().with_context(values, context)
        }
        RuntimeIssue::Delivery(failure) => {
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
            failure
                .error()
                .diagnostic(values)?
                .with_context(values, context)
        }
        RuntimeIssue::Maintenance(failure) => {
            maintenance_failure_diagnostic(values, failure.id(), failure.kind(), failure.message())
        }
        RuntimeIssue::Obligation(_) => unreachable!("obligations render per acceptor"),
    }
}

/// What names a waiting work in a report.
fn work_args(
    values: &Values,
    work: &RuntimeWork,
) -> Result<Vec<(&'static str, HandleValue)>, Error> {
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
    Ok(args)
}

fn machine_path(values: &Values, keys: &[Key]) -> Result<HandleValue, Error> {
    values.list(keys.iter().map(|key| values.key(key)))
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

    use super::maintenance_failure_diagnostic;

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
