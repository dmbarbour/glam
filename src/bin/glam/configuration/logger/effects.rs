use std::io::{self, Write};
use std::sync::Arc;

use bytes::Bytes;
use glam::reflection::{
    CommitResult, EffectRequestSpec, HostSnapshot, ReflectionJournal, ReflectionQueryWriter,
    ReflectionRequest, ReflectionServices, ReflectionTransaction, RequestContext, RequestResult,
    TaskCommit, TaskEnvironment, TaskHost, TaskSpecialization, handle_reflection_request,
    reflection_request_specs,
};
use glam::{
    Assembler, Diagnostic, DiagnosticBus, DiagnosticIngress, Error, RuntimeDeliveryOutcome,
    RuntimeEventJournal, RuntimeEventSnapshot, RuntimeInputReader, RuntimeOutputDelivery,
    RuntimeOutputWriter, RuntimeTaskCapability, Value,
};

use super::supervisor::LogHost;
use crate::DiagnosticBusLocal;

#[derive(Clone)]
pub(crate) struct MainEffects {
    assembler: Assembler,
}

impl MainEffects {
    pub(crate) fn new(assembler: Assembler) -> Self {
        Self { assembler }
    }
}

#[derive(Clone)]
pub(crate) enum MainRequest {
    Reflection(ReflectionRequest),
    ReadLog,
    WriteStderr,
}

type MainSnapshot = RuntimeEventSnapshot;

#[derive(Clone, Default)]
pub(crate) struct MainJournal {
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
                    .map_err(glam::reflection::TaskHalt::from)?
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
            return Diagnostic::from_transport_value(&context.host().resources.values(), &value)
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
        let value = Diagnostic::from_transport_value(&context.host().resources.values(), &value)
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

pub(crate) struct LoggerTaskHost {
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
    pub(crate) fn new(
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
        let decode_values = input.runtime.values();
        let diagnostic_output = input
            .runtime
            .output_endpoint(
                move |value| Diagnostic::from_transport_value(&decode_values, &value),
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
                        .as_bytes()?
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use glam::reflection::ReflectionServices;
    use glam::{
        Assembler, Diagnostic, DiagnosticBus, DiagnosticEvent, DiagnosticSubscriber,
        EffectTokenDomain, EvaluationRuntime, RuntimeEventJournal, Severity,
    };

    use super::{LoggerTaskHost, MainJournal};
    use crate::configuration::logger::LogHost;

    struct Capture(Arc<Mutex<Vec<DiagnosticEvent>>>);

    impl DiagnosticSubscriber for Capture {
        fn receive(&self, event: DiagnosticEvent) {
            self.0
                .lock()
                .expect("output capture should not be poisoned")
                .push(event);
        }
    }

    fn assert_logger_task_owner_inventory(host: &LoggerTaskHost, journal: &MainJournal) {
        let LoggerTaskHost {
            resources: _,
            _diagnostic_ingress: _,
            diagnostic_reader: _,
            diagnostics: _,
            reflection_environment,
            diagnostic_writer: _,
            diagnostic_delivery: _,
            stderr_writer: _,
            stderr_delivery: _,
        } = host;
        let _: &glam::Value = reflection_environment;
        let MainJournal {
            reflection: _,
            events: _,
        } = journal;
    }

    #[test]
    fn logger_task_owner_inventory_is_compile_exhaustive() {
        let _: fn(&LoggerTaskHost, &MainJournal) = assert_logger_task_owner_inventory;
    }

    #[test]
    fn logger_task_host_retires_its_reflection_environment_root_exactly() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let input_diagnostics = DiagnosticBus::for_runtime(&runtime);
        let input = Arc::new(LogHost::with_runtime(runtime.clone(), &input_diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .build()
            .expect("logger assembler should build");
        let domain = EffectTokenDomain::new(&runtime.values());
        let payload = Arc::new(());
        let retained = Arc::downgrade(&payload);
        let host = LoggerTaskHost::new(
            input,
            DiagnosticBus::for_runtime(&runtime),
            domain.issue(payload),
            assembler,
        );

        assert!(retained.upgrade().is_some());
        drop(host);
        assert!(retained.upgrade().is_none());
    }

    #[test]
    fn logger_session_output_is_separate_from_assembler_input() {
        let input_diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&input_diagnostics));
        let diagnostics = DiagnosticBus::new();
        let output = Arc::new(Mutex::new(Vec::new()));
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
