use std::sync::Arc;

use glam::reflection::EffectLifecycle;
use glam::{
    Diagnostic, DiagnosticBus, DiagnosticIngress, Error, EvaluationRuntime, QuiescenceReport,
    RuntimeDeadlockWork, RuntimeDeliveryFailure, RuntimeDeliveryOutcome, RuntimeDependency,
    RuntimeDisposition, RuntimeDispositionKind, RuntimeEventJournal, RuntimeInputReader,
    RuntimeOutputDelivery, RuntimeOutputWriter, RuntimeTaskCapability, RuntimeWorkKind,
    RuntimeWorkState, Severity, Value, Values,
};

use crate::rendering::DefaultLogger;

pub(crate) struct LogHost {
    pub(crate) runtime: EvaluationRuntime,
    pub(crate) task_capability: Arc<RuntimeTaskCapability>,
    pub(crate) diagnostic_ingress: DiagnosticIngress,
    pub(crate) diagnostic_reader: RuntimeInputReader,
}

/// Host ownership for one long-lived diagnostic ingress and a sequence of
/// configured logger lifecycles. Rearming changes only the coordinator root;
/// publications keep flowing through the original ingress and bus sequence.
pub(crate) struct LoggerSupervisor {
    input: Arc<LogHost>,
    fallback_writer: RuntimeOutputWriter,
    pub(crate) fallback_delivery: RuntimeOutputDelivery<Diagnostic>,
    state: std::sync::Mutex<LoggerSupervisorState>,
}

struct LoggerSupervisorState {
    next_generation: u64,
    active: Option<LoggerInstallation>,
}

pub(crate) struct SettledReportSelection {
    pub(crate) task_failures: Vec<glam::ReasoningFailure>,
    pub(crate) delivery_failures: Vec<Arc<RuntimeDeliveryFailure>>,
    pub(crate) exit_errors: Vec<RuntimeDisposition>,
    pub(crate) killed_work: Vec<RuntimeDeadlockWork>,
}

#[derive(Clone)]
pub(crate) struct LoggerInstallation {
    pub(crate) generation: u64,
    pub(crate) lifecycle: EffectLifecycle,
}

impl LoggerSupervisor {
    pub(crate) fn new<F>(input: Arc<LogHost>, fallback: F) -> Self
    where
        F: Fn(Diagnostic) + Send + Sync + 'static,
    {
        Self::new_fallible(input, move |diagnostic| {
            fallback(diagnostic);
            Ok(())
        })
    }

    pub(crate) fn new_fallible<F>(input: Arc<LogHost>, fallback: F) -> Self
    where
        F: Fn(Diagnostic) -> Result<(), Error> + Send + Sync + 'static,
    {
        let decode_values = input.runtime.values();
        let endpoint = input
            .runtime
            .output_endpoint(
                move |value| Diagnostic::from_transport_value(&decode_values, &value),
                fallback,
            )
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

    pub(crate) fn install(&self) -> Result<LoggerInstallation, Error> {
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

    pub(crate) fn finish(&self, installation: &LoggerInstallation) {
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

    pub(crate) fn fallback_and_deliver(&self) -> Result<usize, Error> {
        let transferred = self.input.diagnostic_ingress.fallback()?;
        self.deliver_fallback()?;
        Ok(transferred)
    }

    pub(crate) fn deliver_fallback(&self) -> Result<(), Error> {
        deliver_fallback_output(&self.fallback_delivery)
    }

    pub(crate) fn render_settled_report(
        &self,
        report: &mut QuiescenceReport,
    ) -> Result<usize, Error> {
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
    pub(crate) fn active_status(&self) -> Option<glam::reflection::EffectLifecycleStatus> {
        self.state
            .lock()
            .expect("logger supervisor mutex should not be poisoned")
            .active
            .as_ref()
            .map(|active| active.lifecycle.status())
    }
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
            Diagnostic::from_emission(values, Severity::Error, message.clone())?
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
impl LogHost {
    #[cfg(test)]
    pub(crate) fn new(diagnostics: &DiagnosticBus) -> Self {
        let runtime =
            EvaluationRuntime::new(0).expect("test logger runtime should be constructible");
        Self::with_runtime(runtime, diagnostics)
    }

    pub(crate) fn with_runtime(runtime: EvaluationRuntime, diagnostics: &DiagnosticBus) -> Self {
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

    pub(crate) fn drain_default(&self, logger: &DefaultLogger) {
        while let Some(diagnostic) = self.take_diagnostic() {
            logger.emit(&diagnostic);
        }
    }

    pub(crate) fn take_diagnostic(&self) -> Option<Diagnostic> {
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
                            Diagnostic::from_transport_value(&self.runtime.values(), &value)
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use glam::{Diagnostic, DiagnosticBus, Severity};

    use super::{
        LogHost, LoggerInstallation, LoggerSupervisor, LoggerSupervisorState,
        SettledReportSelection,
    };
    use crate::DiagnosticBusLocal;

    fn assert_logger_supervisor_owner_inventory(
        host: &LogHost,
        supervisor: &LoggerSupervisor,
        state: &LoggerSupervisorState,
        selection: &SettledReportSelection,
        installation: &LoggerInstallation,
    ) {
        let LogHost {
            runtime: _,
            task_capability: _,
            diagnostic_ingress: _,
            diagnostic_reader: _,
        } = host;
        let LoggerSupervisor {
            input: _,
            fallback_writer: _,
            fallback_delivery: _,
            state: _,
        } = supervisor;
        let LoggerSupervisorState {
            next_generation: _,
            active: _,
        } = state;
        let SettledReportSelection {
            task_failures: _,
            delivery_failures: _,
            exit_errors: _,
            killed_work: _,
        } = selection;
        let LoggerInstallation {
            generation: _,
            lifecycle: _,
        } = installation;
    }

    #[test]
    fn logger_supervisor_owner_inventory_is_compile_exhaustive() {
        let _: fn(
            &LogHost,
            &LoggerSupervisor,
            &LoggerSupervisorState,
            &SettledReportSelection,
            &LoggerInstallation,
        ) = assert_logger_supervisor_owner_inventory;
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
        let fallback = Arc::new(Mutex::new(Vec::new()));
        let fallback_values = fallback.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            fallback_values
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .push(diagnostic.message().to_owned());
        });
        let values = input.runtime.values();

        let first = supervisor.install().expect("first logger should install");
        assert!(matches!(
            supervisor.active_status(),
            Some(glam::reflection::EffectLifecycleStatus::Launched)
        ));
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
}
