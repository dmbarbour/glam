use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use bytes::Bytes;

use super::*;
use crate::Severity;
use crate::api::{Assembler, Diagnostic, Error as ApiError, EvaluationRuntime, TestValueFacade};
use crate::evaluation::{
    EvaluationSessionRun, EvaluationTaskCancellation, EvaluationTaskHandle, EvaluationTaskStatus,
    ReflectionTaskLauncher, ReflectionTaskResultPolicy, TaskStatusPublisher, TaskStatusWake,
};
use crate::reflection::lifecycle::{run_composed_effect_task, task_launcher};
use crate::reflection::{
    EffectLifecycle, EffectLifecycleStatus, EffectLifecycleTerminal, EffectRun,
    EvaluationQueryHandle, ExactConflictAnalysis, IsolatedEffectSearch, IsolatedSearchPoll,
    ReasoningSessionId, ReflectionEffects, ReflectionHost, ReflectionJournal,
    ReflectionQueryMutation, ReflectionQueryWriter, ReflectionRequest, ReflectionServices,
    ReflectionStore, ReflectionTransaction, StandardEffects, StoreCommitResult, TaskEnvironment,
    handle_reflection_request, reflection_request_specs,
};

fn public_record<I, S>(assembler: &Assembler, entries: I) -> PublicValue
where
    I: IntoIterator<Item = (S, PublicValue)>,
    S: AsRef<str>,
{
    assembler
        .values()
        .record(entries)
        .expect("test record should belong to one runtime")
}

fn public_list(assembler: &Assembler, items: impl IntoIterator<Item = PublicValue>) -> PublicValue {
    assembler
        .values()
        .list(items)
        .expect("test list should belong to one runtime")
}

#[derive(Clone)]
struct TestEffects;

struct CountingLauncher {
    inner: Arc<dyn ReflectionTaskLauncher>,
    builds: Arc<AtomicUsize>,
}

impl ReflectionTaskLauncher for CountingLauncher {
    fn build(
        &self,
        context: EvalContext,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>> {
        self.builds.fetch_add(1, Ordering::AcqRel);
        self.inner.build(context, effect, result_policy)
    }
}

struct ExitCapableLauncher {
    host: Arc<TestHost>,
}

impl ReflectionTaskLauncher for ExitCapableLauncher {
    fn build(
        &self,
        context: EvalContext,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>> {
        let task = EffectTask::new_exit_in_context(effect, TestEffects, self.host.clone(), context)
            .map_err(TaskHalt::into_failure)?;
        Ok(match result_policy {
            ReflectionTaskResultPolicy::RequireUnit => Box::new(UnitEffectTask(
                task.asserting_unit_result(Arc::from("reflection annotation result")),
            )),
            ReflectionTaskResultPolicy::ReturnValue => Box::new(ValueEffectTask(task)),
        })
    }
}

#[derive(Clone)]
enum TestRequest {
    Reflection(ReflectionRequest),
    ReadLog,
    WriteStderr,
    Alternatives,
    Evaluate,
}

#[derive(Clone)]
struct TestSnapshot {
    diagnostics: Arc<[Diagnostic]>,
    revision: u64,
}

#[derive(Clone, Default)]
struct TestJournal {
    reflection: ReflectionJournal,
    consumed_diagnostics: usize,
    stderr: Vec<Bytes>,
}

impl ReflectionTransaction for TestJournal {
    fn reflection_journal(&mut self) -> &mut ReflectionJournal {
        &mut self.reflection
    }
}

impl TaskSpecialization for TestEffects {
    type Host = TestHost;
    type Request = TestRequest;
    type Snapshot = TestSnapshot;
    type Journal = TestJournal;

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        reflection_request_specs()
            .into_iter()
            .map(|request| request.map_request(TestRequest::Reflection))
            .chain([
                EffectRequestSpec::new(
                    "read_log",
                    ["reflection_test", "request", "read_log"],
                    0,
                    TestRequest::ReadLog,
                ),
                EffectRequestSpec::new(
                    "write_stderr",
                    ["reflection_test", "request", "write_stderr"],
                    1,
                    TestRequest::WriteStderr,
                ),
                EffectRequestSpec::new(
                    "alternatives",
                    ["reflection_test", "request", "alternatives"],
                    0,
                    TestRequest::Alternatives,
                ),
                EffectRequestSpec::new(
                    "evaluate",
                    ["reflection_test", "request", "evaluate"],
                    1,
                    TestRequest::Evaluate,
                ),
            ])
            .collect()
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<PublicValue>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskHalt> {
        context
            .host()
            .probe_callback_boundary(CallbackProbeKind::Specialization);
        match request {
            TestRequest::Reflection(request) => {
                handle_reflection_request(request, arguments, context)
            }
            TestRequest::ReadLog => read_test_log(context),
            TestRequest::WriteStderr => {
                let [value]: [PublicValue; 1] = arguments
                    .try_into()
                    .map_err(|_| TaskHalt::new("test stderr request received the wrong arity"))?;
                let bytes = value_bytes(value.as_core())?;
                if let Some(mut transaction) = context.transaction() {
                    transaction.parts().1.stderr.push(bytes);
                } else {
                    context.host().write_stderr(bytes);
                    context.committed();
                }
                Ok(RequestResult::ReturnUnit)
            }
            TestRequest::Alternatives => Ok(RequestResult::Alternatives(vec![
                PublicValue::from_core(
                    context.eval_context().values(),
                    Value::binary_from_text("first"),
                ),
                PublicValue::from_core(
                    context.eval_context().values(),
                    Value::binary_from_text("second"),
                ),
            ])),
            TestRequest::Evaluate => {
                let [value]: [PublicValue; 1] = arguments
                    .try_into()
                    .map_err(|_| TaskHalt::new("test evaluate request received the wrong arity"))?;
                Ok(RequestResult::Return(
                    context.evaluate(&value)?.into_value(),
                ))
            }
        }
    }
}

fn read_test_log(context: &mut RequestContext<'_, TestEffects>) -> Result<RequestResult, TaskHalt> {
    if let Some(generation) = context.transaction_generation() {
        let values = context.eval_context().values().clone();
        context.observe_host_generation(generation);
        let mut transaction = context
            .transaction()
            .expect("checked active reflection transaction");
        let (snapshot, journal) = transaction.parts();
        let Some(diagnostic) = snapshot.diagnostics.get(journal.consumed_diagnostics) else {
            return Ok(RequestResult::Fail);
        };
        journal.consumed_diagnostics += 1;
        return diagnostic
            .enrich_with_factory(&values)
            .map(RequestResult::Return)
            .map_err(TaskHalt::from);
    }

    loop {
        let snapshot = <TestHost as TaskHost<TestEffects>>::snapshot(context.host());
        context.observe_host_generation(snapshot.generation());
        let Some(diagnostic) = snapshot.extra().diagnostics.first() else {
            return Ok(RequestResult::Fail);
        };
        let value = diagnostic
            .enrich_with_factory(context.eval_context().values())
            .map_err(TaskHalt::from)?;
        let commit = TaskCommit::new(
            StoreJournal::new(snapshot.store().clone()),
            snapshot.extra().clone(),
            TestJournal {
                reflection: ReflectionJournal::default(),
                consumed_diagnostics: 1,
                stderr: Vec::new(),
            },
        );
        match <TestHost as TaskHost<TestEffects>>::commit(context.host(), commit) {
            CommitResult::Committed => {
                context.committed();
                return Ok(RequestResult::Return(value));
            }
            CommitResult::Conflict => {}
            CommitResult::MissingVolume(volume) => {
                return Err(missing_volume_error(volume));
            }
            CommitResult::Closed => return Ok(RequestResult::Cancelled),
        }
    }
}

#[derive(Default)]
struct TestHost {
    reasoning_session: Option<ReasoningSessionId>,
    state: Arc<Mutex<TestHostState>>,
}

struct TestQueryWriter {
    state: Arc<Mutex<TestHostState>>,
}

struct TestHostState {
    generation: u64,
    extra_revision: u64,
    store: ReflectionStore,
    diagnostics: Vec<Diagnostic>,
    stderr: Vec<Bytes>,
    wake_diagnostic: Option<Diagnostic>,
    wake_heap: Option<PublicValue>,
    wait_count: usize,
    callback_probe: bool,
    callback_probe_counts: [usize; 3],
    closed: bool,
}

#[derive(Clone, Copy)]
enum CallbackProbeKind {
    Snapshot = 0,
    Commit = 1,
    Specialization = 2,
}

impl Default for TestHostState {
    fn default() -> Self {
        Self {
            generation: 1,
            extra_revision: 0,
            store: ReflectionStore::new(
                crate::core::test_value_factory(),
                Arc::new(ExactConflictAnalysis),
            ),
            diagnostics: Vec::new(),
            stderr: Vec::new(),
            wake_diagnostic: None,
            wake_heap: None,
            wait_count: 0,
            callback_probe: false,
            callback_probe_counts: [0; 3],
            closed: false,
        }
    }
}

impl TestHost {
    fn with_diagnostics(values: CoreValueFactory, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            reasoning_session: None,
            state: Arc::new(Mutex::new(TestHostState {
                store: ReflectionStore::new(values, Arc::new(ExactConflictAnalysis)),
                diagnostics,
                ..TestHostState::default()
            })),
        }
    }

    fn with_wake_diagnostic(values: CoreValueFactory, diagnostic: Diagnostic) -> Self {
        Self {
            reasoning_session: None,
            state: Arc::new(Mutex::new(TestHostState {
                store: ReflectionStore::new(values, Arc::new(ExactConflictAnalysis)),
                wake_diagnostic: Some(diagnostic),
                ..TestHostState::default()
            })),
        }
    }

    fn with_wake_heap(values: CoreValueFactory, heap: PublicValue) -> Self {
        Self {
            reasoning_session: None,
            state: Arc::new(Mutex::new(TestHostState {
                store: ReflectionStore::new(values, Arc::new(ExactConflictAnalysis)),
                wake_heap: Some(heap),
                ..TestHostState::default()
            })),
        }
    }

    fn with_values(values: CoreValueFactory) -> Self {
        Self {
            reasoning_session: None,
            state: Arc::new(Mutex::new(TestHostState {
                store: ReflectionStore::new(values, Arc::new(ExactConflictAnalysis)),
                ..TestHostState::default()
            })),
        }
    }

    fn with_callback_probe(values: CoreValueFactory) -> Self {
        Self {
            reasoning_session: None,
            state: Arc::new(Mutex::new(TestHostState {
                store: ReflectionStore::new(values, Arc::new(ExactConflictAnalysis)),
                callback_probe: true,
                ..TestHostState::default()
            })),
        }
    }

    fn probe_callback_boundary(&self, kind: CallbackProbeKind) {
        let values = {
            let mut state = self.state.lock().unwrap();
            if !state.callback_probe {
                return;
            }
            state.callback_probe_counts[kind as usize] += 1;
            state.store.values().clone()
        };
        values
            .collect_managed_for_test()
            .expect("effect interpreter callback must not inherit a mutator");
    }

    fn callback_probe_count(&self, kind: CallbackProbeKind) -> usize {
        self.state.lock().unwrap().callback_probe_counts[kind as usize]
    }

    fn stderr(&self) -> Vec<Bytes> {
        self.state.lock().unwrap().stderr.clone()
    }

    fn heap(&self) -> PublicValue {
        self.state.lock().unwrap().store.root().clone()
    }

    fn diagnostics(&self) -> Vec<Diagnostic> {
        self.state.lock().unwrap().diagnostics.clone()
    }

    fn wait_count(&self) -> usize {
        self.state.lock().unwrap().wait_count
    }

    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        {
            let mut state = self.state.lock().unwrap();
            state.diagnostics.push(diagnostic);
            state.extra_revision += 1;
            state.generation += 1;
        }
        self.publish_runtime_observation();
    }

    fn write_stderr(&self, bytes: Bytes) {
        self.state.lock().unwrap().stderr.push(bytes);
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        let (changed, result) = {
            let mut state = self.state.lock().unwrap();
            state.wait_count += 1;
            if state.generation != observed_generation {
                (false, true)
            } else if let Some(heap) = state.wake_heap.take() {
                state.store.replace_root(heap);
                state.generation += 1;
                (true, true)
            } else if let Some(diagnostic) = state.wake_diagnostic.take() {
                state.diagnostics.push(diagnostic);
                state.extra_revision += 1;
                state.generation += 1;
                (true, true)
            } else {
                (false, false)
            }
        };
        if changed {
            self.publish_runtime_observation();
        }
        result
    }

    fn publish_runtime_observation(&self) {
        let coordinator = self.state.lock().unwrap().store.values().work_coordinator();
        if let Some(coordinator) = coordinator {
            coordinator.publish_runtime_observation();
        }
    }
}

impl TaskEnvironment for TestHost {
    fn reflection_environment(&self) -> PublicValue {
        let state = self.state.lock().unwrap();
        let values = state.store.values();
        let process_environment = PublicValue::from_core(
            values,
            Value::Dict(Dict::new_sync().insert(
                Key::binary_from_text("GLAM_TEST_ENV"),
                Value::binary_from_text("present"),
            )),
        );
        PublicValue::from_core(
            values,
            Value::Dict(
                Dict::new_sync()
                    .insert(
                        Key::atom_from_text("glam"),
                        Value::Dict(Dict::new_sync().insert(
                            Key::atom_from_text("version"),
                            Value::binary_from_text(env!("CARGO_PKG_VERSION")),
                        )),
                    )
                    .insert(
                        Key::atom_from_text("process"),
                        Value::Dict(
                            Dict::new_sync()
                                .insert(
                                    Key::atom_from_text("args"),
                                    Value::List(List::from_values(vec![
                                        Value::binary_from_text("glam"),
                                        Value::binary_from_text("--test"),
                                    ])),
                                )
                                .insert(
                                    Key::atom_from_text("env"),
                                    process_environment.as_core().clone(),
                                ),
                        ),
                    ),
            ),
        )
    }
}

impl ReflectionServices for TestHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        TestHost::emit_diagnostic(self, diagnostic);
    }

    fn query_writer(&self) -> Option<Arc<dyn ReflectionQueryWriter>> {
        Some(Arc::new(TestQueryWriter {
            state: self.state.clone(),
        }))
    }
}

impl ReflectionQueryWriter for TestQueryWriter {
    fn update_query_guarded(
        &self,
        _mutation: ReflectionQueryMutation<'_>,
        handle: &Arc<EvaluationQueryHandle>,
        result: PublicValue,
    ) -> Box<dyn FnOnce() + Send> {
        let updated = {
            let mut state = self.state.lock().unwrap();
            let updated = state.store.update_query(handle, result);
            if updated {
                state.generation += 1;
            }
            updated
        };
        let coordinator = updated
            .then(|| self.state.lock().unwrap().store.values().work_coordinator())
            .flatten();
        Box::new(move || {
            if let Some(coordinator) = coordinator {
                coordinator.publish_runtime_observation();
            }
        })
    }
}

impl TaskHost<TestEffects> for TestHost {
    fn snapshot(&self) -> HostSnapshot<TestEffects> {
        self.probe_callback_boundary(CallbackProbeKind::Snapshot);
        let state = self.state.lock().unwrap();
        HostSnapshot::new(
            state.generation,
            state.store.snapshot(),
            TestSnapshot {
                diagnostics: Arc::from(state.diagnostics.clone()),
                revision: state.extra_revision,
            },
        )
    }

    fn commit(&self, commit: TaskCommit<TestEffects>) -> CommitResult {
        self.probe_callback_boundary(CallbackProbeKind::Commit);
        let (store, snapshot, journal) = commit.into_parts();
        {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return CommitResult::Closed;
            }
            if (journal.consumed_diagnostics != 0 && state.extra_revision != snapshot.revision)
                || state.diagnostics.len() < journal.consumed_diagnostics
            {
                return CommitResult::Conflict;
            }
            match state.store.try_commit(&store) {
                StoreCommitResult::Committed => {}
                StoreCommitResult::Conflict => return CommitResult::Conflict,
                StoreCommitResult::MissingVolume(volume) => {
                    return CommitResult::MissingVolume(volume);
                }
            }
            let consumed = journal.consumed_diagnostics;
            state.diagnostics.drain(..consumed);
            state
                .diagnostics
                .extend(journal.reflection.diagnostics().iter().cloned());
            state.stderr.extend_from_slice(&journal.stderr);
            if consumed != 0 || !journal.reflection.diagnostics().is_empty() {
                state.extra_revision += 1;
            }
            state.generation += 1;
        }
        journal.reflection.commit_updates();
        self.publish_runtime_observation();
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        TestHost::wait_for_change(self, observed_generation)
    }
}

impl TaskHost<StandardEffects> for TestHost {
    fn snapshot(&self) -> HostSnapshot<StandardEffects> {
        let state = self.state.lock().unwrap();
        HostSnapshot::new(state.generation, state.store.snapshot(), ())
    }

    fn commit(&self, commit: TaskCommit<StandardEffects>) -> CommitResult {
        let (store, _snapshot, _journal) = commit.into_parts();
        {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return CommitResult::Closed;
            }
            match state.store.try_commit(&store) {
                StoreCommitResult::Committed => {}
                StoreCommitResult::Conflict => return CommitResult::Conflict,
                StoreCommitResult::MissingVolume(volume) => {
                    return CommitResult::MissingVolume(volume);
                }
            }
            state.generation += 1;
        }
        self.publish_runtime_observation();
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        TestHost::wait_for_change(self, observed_generation)
    }
}

impl TaskHost<ReflectionEffects> for TestHost {
    fn snapshot(&self) -> HostSnapshot<ReflectionEffects> {
        let state = self.state.lock().unwrap();
        HostSnapshot::new(state.generation, state.store.snapshot(), ())
    }

    fn commit(&self, commit: TaskCommit<ReflectionEffects>) -> CommitResult {
        let (store, _snapshot, journal) = commit.into_parts();
        {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return CommitResult::Closed;
            }
            match state.store.try_commit(&store) {
                StoreCommitResult::Committed => {}
                StoreCommitResult::Conflict => return CommitResult::Conflict,
                StoreCommitResult::MissingVolume(volume) => {
                    return CommitResult::MissingVolume(volume);
                }
            }
            state
                .diagnostics
                .extend(journal.diagnostics().iter().cloned());
            if !journal.diagnostics().is_empty() {
                state.extra_revision += 1;
            }
            state.generation += 1;
        }
        journal.commit_updates();
        self.publish_runtime_observation();
        CommitResult::Committed
    }

    fn reasoning_session_id(&self) -> Option<ReasoningSessionId> {
        self.reasoning_session
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        TestHost::wait_for_change(self, observed_generation)
    }
}

fn value_bytes(value: &Value) -> Result<Bytes, TaskHalt> {
    let context = EvalContext::standalone();
    match evaluate(&context, value.clone())? {
        Value::Binary(bytes) => Ok(bytes),
        Value::List(list) => eval::list_output_bytes(&context, &list)
            .map(Bytes::from)
            .map_err(TaskHalt::from),
        _ => Err(TaskHalt::new("test stderr request requires binary data")),
    }
}

fn run_log_test(
    assembler: &Assembler,
    effect: &PublicValue,
    host: Arc<TestHost>,
) -> Result<TaskOutcome, TaskHalt> {
    run_composed_effect_task(EffectTask::new_owned_in_context(
        effect.as_core().clone(),
        TestEffects,
        host,
        EvalContext::isolated(assembler.core_values()),
    )?)
}

fn run_log_test_with_fusion(
    assembler: &Assembler,
    effect: &PublicValue,
    host: Arc<TestHost>,
    force_unfused: bool,
) -> (Result<TaskOutcome, TaskHalt>, Arc<EffectPhaseProbe>) {
    let probe = Arc::new(EffectPhaseProbe::default());
    let mut task = EffectTask::new_owned_in_context(
        effect.as_core().clone(),
        TestEffects,
        host,
        EvalContext::isolated(assembler.core_values()),
    )
    .expect("fusion fixture should construct")
    .with_phase_probe(probe.clone());
    if force_unfused {
        task = task.forcing_unfused();
    }
    (task.run(), probe)
}

fn fusion_result_bytes(
    assembler: &Assembler,
    result: Result<TaskOutcome, TaskHalt>,
) -> Result<Vec<u8>, String> {
    match result {
        Ok(TaskOutcome::Complete(value)) => assembler
            .to_binary(&value)
            .map(|bytes| bytes.to_vec())
            .map_err(|error| error.to_string()),
        Ok(TaskOutcome::Cancelled) => Err("cancelled".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

fn isolated_fusion_bytes(
    assembler: &Assembler,
    effect: &PublicValue,
    force_unfused: bool,
) -> Vec<Vec<u8>> {
    let owned = EvalContext::isolated(assembler.core_values());
    let (context, owner) = owned.into_parts();
    let mut task = EffectTask::new_in_context_with_policy(
        effect.as_core().clone(),
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
        context,
        true,
    )
    .expect("isolated fusion fixture should construct");
    task._demand_owner = Some(owner);
    if force_unfused {
        task = task.forcing_unfused();
    }
    task.run().expect("isolated fusion fixture should finish");
    task.completed_search()
        .expect("isolated fusion fixture should retain its branches")
        .iter()
        .filter_map(IsolatedSearchBranch::value)
        .map(|value| assembler.to_binary(value).unwrap().to_vec())
        .collect()
}

fn run_reflection_test(
    assembler: &Assembler,
    effect: &PublicValue,
    host: Arc<TestHost>,
) -> Result<TaskOutcome, TaskHalt> {
    let host: Arc<dyn ReflectionHost<ReflectionEffects>> = host;
    run_composed_effect_task(EffectTask::new_owned_in_context(
        effect.as_core().clone(),
        ReflectionEffects,
        host,
        EvalContext::isolated(assembler.core_values()),
    )?)
}

fn run_standard_test(assembler: &Assembler, effect: &PublicValue) -> Result<TaskOutcome, TaskHalt> {
    run_composed_effect_task(EffectTask::new_owned_in_context(
        effect.as_core().clone(),
        StandardEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
        EvalContext::isolated(assembler.core_values()),
    )?)
}

fn run_standard_on(
    assembler: &Assembler,
    effect: &PublicValue,
    host: Arc<TestHost>,
) -> Result<TaskOutcome, TaskHalt> {
    run_composed_effect_task(EffectTask::new_owned_in_context(
        effect.as_core().clone(),
        StandardEffects,
        host,
        EvalContext::isolated(assembler.core_values()),
    )?)
}

fn compile_effect(source: &str) -> (Assembler, PublicValue) {
    let runtime = crate::api::EvaluationRuntime::new(0).expect("test runtime should build");
    compile_effect_with_runtime(&runtime, source)
}

fn compile_effect_with_runtime(
    runtime: &crate::api::EvaluationRuntime,
    source: &str,
) -> (Assembler, PublicValue) {
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("test assembler should build");
    let module = assembler
        .module(["reflection_test"])
        .script(
            "g",
            format!("language g0\nimport 'std\nrefl.effect = {source}\n"),
        )
        .build()
        .expect("effect fixture should compile");
    let effect = assembler
        .get(module.value(), "refl.effect")
        .expect("effect fixture should define effect");
    (assembler, effect)
}

fn task_halt_contexts(assembler: &Assembler, halt: &TaskHalt) -> Vec<Value> {
    let diagnostic = eval::failure_diagnostic_value_with(
        &assembler.core_values(),
        halt.clone().into_failure().as_ref(),
    );
    let context = assembler.eval_context();
    let Value::Dict(diagnostic) = eval::eval_value(&context, &diagnostic).unwrap() else {
        panic!("task halt diagnostic must be a dictionary")
    };
    let message = eval::eval_value(
        &context,
        diagnostic
            .get(&*keys::MSG)
            .expect("task halt diagnostic should define msg"),
    )
    .unwrap();
    let Value::Dict(message) = message else {
        panic!("task halt msg must be a dictionary")
    };
    let contexts = eval::eval_value(
        &context,
        message
            .get(&*keys::CONTEXT)
            .expect("task halt diagnostic should define msg.context"),
    )
    .unwrap();
    let Value::List(contexts) = contexts else {
        panic!("task halt msg.context must be a list")
    };
    eval::list_to_value_items(&context, &contexts).unwrap()
}

fn assert_list_values(assembler: &Assembler, actual: &PublicValue, expected: &PublicValue) {
    let actual = assembler.evaluate(actual).unwrap();
    let Value::List(actual) = actual.as_core() else {
        panic!("actual value should be a list")
    };
    let Value::List(expected) = expected.as_core() else {
        panic!("expected value should be a list")
    };
    assert_eq!(
        eval::list_to_value_items(&assembler.eval_context(), actual).unwrap(),
        eval::list_to_value_items(&assembler.eval_context(), expected).unwrap(),
    );
}

fn completed(source: &str) -> (Assembler, PublicValue) {
    let (assembler, effect) = compile_effect(source);
    let TaskOutcome::Complete(value) = run_standard_test(&assembler, &effect).unwrap() else {
        panic!("finite effect should complete")
    };
    (assembler, value)
}

fn poll_isolated_search<S: TaskSpecialization>(
    search: &mut IsolatedEffectSearch<S>,
) -> Arc<[IsolatedSearchBranch<S>]> {
    loop {
        match search.poll(256) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Complete(results) => return results,
            IsolatedSearchPoll::Blocked(blocked) => panic!(
                "finite isolated search blocked: dependency={}, generation={:?}, error={:?}",
                blocked.waiting_on_dependency(),
                blocked.observed_generation(),
                blocked.error()
            ),
            IsolatedSearchPoll::Failed(error) => {
                panic!("finite isolated search failed: {error}")
            }
            IsolatedSearchPoll::Cancelled => panic!("finite isolated search was cancelled"),
        }
    }
}

fn isolated_standard_results(source: &str) -> (Assembler, Vec<PublicValue>) {
    let (assembler, effect) = compile_effect(source);
    let host: Arc<dyn TaskHost<StandardEffects>> =
        Arc::new(TestHost::with_values(assembler.core_values()));
    let mut search = IsolatedEffectSearch::new(
        &assembler.evaluation_runtime(),
        &effect,
        StandardEffects,
        host,
    )
    .expect("isolated effect should initialize");
    let results = poll_isolated_search(&mut search);
    let values = results
        .iter()
        .filter_map(|result| result.value().cloned())
        .collect();
    (assembler, values)
}

fn assert_search_bytes(source: &str, expected: &[&[u8]]) {
    let (assembler, values) = isolated_standard_results(source);
    let actual = values
        .iter()
        .map(|value| assembler.to_binary(value).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn isolated_search_obeys_ordered_choice_laws() {
    assert_search_bytes(".r \"single\"", &[b"single"]);
    assert_search_bytes(".fail", &[]);
    assert_search_bytes(".cut (.fail)", &[]);
    assert_search_bytes(
        ".alt (.r \"left\") (.alt (.fail) (.r \"right\"))",
        &[b"left", b"right"],
    );
    assert_search_bytes(
        "(.alt (.r \"A\") (.r \"B\")) >>= (\\x -> .alt (.r (x ++ \"1\")) (.r (x ++ \"2\")))",
        &[b"A1", b"A2", b"B1", b"B2"],
    );
    assert_search_bytes(
        ".alt (.cut (.alt (.r \"first\") (.r \"discarded\"))) (.r \"outer\")",
        &[b"first", b"outer"],
    );
    assert_search_bytes(
        ".fix (\\_loop -> .alt (.r \"fix left\") (.r \"fix right\"))",
        &[b"fix left", b"fix right"],
    );
    assert_search_bytes(
        ".alt (.reset \"prompt\" (.shift \"prompt\" (\\continuation -> continuation \"resumed\"))) (.r \"outer\")",
        &[b"resumed", b"outer"],
    );
}

#[test]
fn specialized_requests_can_resume_each_ordered_alternative() {
    let (assembler, effect) =
        compile_effect(".alternatives >>= (\\value -> .r (value ++ \" result\"))");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut search =
        IsolatedEffectSearch::new(&assembler.evaluation_runtime(), &effect, TestEffects, host)
            .expect("isolated effect should initialize");
    let results = poll_isolated_search(&mut search);
    let values = results
        .iter()
        .filter_map(IsolatedSearchBranch::value)
        .map(|value| assembler.to_binary(value).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values, [b"first result".as_slice(), b"second result"]);
}

#[test]
fn effect_interpreter_callbacks_do_not_inherit_evaluator_mutators() {
    let (assembler, effect) = compile_effect(
        ".cut (.evaluate (\"left\" ++ \"right\") >>= (\\value -> (.heap.set ['value] value) =>> .r value))",
    );
    let host = Arc::new(TestHost::with_callback_probe(assembler.core_values()));
    let phase_probe = Arc::new(EffectPhaseProbe::default());

    let task = EffectTask::new_owned_in_context(
        effect.as_core().clone(),
        TestEffects,
        host.clone(),
        EvalContext::isolated(assembler.core_values()),
    )
    .unwrap()
    .with_phase_probe(phase_probe.clone());
    let TaskOutcome::Complete(value) = run_composed_effect_task(task).unwrap() else {
        panic!("bounded interpreter evaluation should complete")
    };

    assert_eq!(
        assembler.to_binary(&value).unwrap(),
        b"leftright".as_slice()
    );
    assert!(host.callback_probe_count(CallbackProbeKind::Snapshot) > 0);
    assert!(host.callback_probe_count(CallbackProbeKind::Commit) > 0);
    assert!(host.callback_probe_count(CallbackProbeKind::Specialization) > 0);
    assert_eq!(
        phase_probe.phase(),
        EffectMachinePhase::ContinuationDelivered as usize,
        "request parsing, mutator-free interpretation, and continuation delivery must occur in order"
    );
}

#[test]
fn fused_standard_chains_match_unfused_results_with_fewer_request_roots() {
    for (source, expected) in [
        (
            ".r \"A\" >>= (\\a -> .r \"B\" >>= (\\b -> .r (a ++ b)))",
            b"AB".as_slice(),
        ),
        (
            ".set ['value] \"state\" =>> .get ['value] >>= (\\value -> .r value)",
            b"state".as_slice(),
        ),
    ] {
        let (assembler, effect) = compile_effect(source);
        let (unfused, unfused_probe) = run_log_test_with_fusion(
            &assembler,
            &effect,
            Arc::new(TestHost::with_values(assembler.core_values())),
            true,
        );
        let (fused, fused_probe) = run_log_test_with_fusion(
            &assembler,
            &effect,
            Arc::new(TestHost::with_values(assembler.core_values())),
            false,
        );

        assert_eq!(fusion_result_bytes(&assembler, unfused).unwrap(), expected);
        assert_eq!(fusion_result_bytes(&assembler, fused).unwrap(), expected);
        assert!(
            fused_probe.fused_requests() > 0,
            "fixture did not fuse: {source}"
        );
        assert!(
            fused_probe.request_roots() < unfused_probe.request_roots(),
            "fusion should root fewer phase-local values for {source}: fused {}, unfused {}",
            fused_probe.request_roots(),
            unfused_probe.request_roots()
        );
    }
}

#[test]
fn fused_standard_chains_resume_at_the_cooperative_budget() {
    let mut statements = String::new();
    for _ in 0..(EFFECT_FUSION_BUDGET + 8) {
        statements.push_str(".r (); ");
    }
    let source = format!("do {{ {statements}.r \"done\" }}");
    let (assembler, effect) = compile_effect(&source);
    let (unfused, _) = run_log_test_with_fusion(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
        true,
    );
    let (fused, probe) = run_log_test_with_fusion(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
        false,
    );
    assert_eq!(
        fusion_result_bytes(&assembler, fused),
        fusion_result_bytes(&assembler, unfused)
    );
    assert!(probe.fused_requests() >= EFFECT_FUSION_BUDGET);
}

#[test]
fn fused_control_boundaries_match_the_unfused_reference() {
    for source in [
        ".fail",
        ".cut (.alt (.fail) (.fail))",
        ".cut (.alt (.fail) (.r \"fallback\"))",
        ".reset \"prompt\" (.shift \"prompt\" (\\continuation -> continuation \"resumed\"))",
        ".fix (\\_loop -> .r \"fixed\")",
    ] {
        let (assembler, effect) = compile_effect(source);
        let (unfused, _) = run_log_test_with_fusion(
            &assembler,
            &effect,
            Arc::new(TestHost::with_values(assembler.core_values())),
            true,
        );
        let (fused, _) = run_log_test_with_fusion(
            &assembler,
            &effect,
            Arc::new(TestHost::with_values(assembler.core_values())),
            false,
        );
        assert_eq!(
            fusion_result_bytes(&assembler, fused),
            fusion_result_bytes(&assembler, unfused),
            "fused control behavior diverged for {source}"
        );
    }
}

#[test]
fn fused_isolated_search_preserves_unfused_branch_order() {
    let (assembler, effect) = compile_effect(
        "(.alt (.r \"A\") (.r \"B\")) >>= (\\value -> .alt (.r (value ++ \"1\")) (.r (value ++ \"2\")))",
    );
    let unfused = isolated_fusion_bytes(&assembler, &effect, true);
    let fused = isolated_fusion_bytes(&assembler, &effect, false);
    assert_eq!(fused, unfused);
    assert_eq!(fused, [b"A1", b"A2", b"B1", b"B2"]);
}

#[test]
fn fused_retry_observations_match_the_unfused_reference() {
    let (assembler, effect) =
        compile_effect(".cut (.heap.get ['handler] >>= (\\handler -> handler ()))");
    let (_, handler) =
        compile_effect_with_runtime(&assembler.evaluation_runtime(), "\\_ -> .r \"recovered\"");
    let heap = public_record(&assembler, [("handler", handler)]);

    let (unfused, _) = run_log_test_with_fusion(
        &assembler,
        &effect,
        Arc::new(TestHost::with_wake_heap(
            assembler.core_values(),
            heap.clone(),
        )),
        true,
    );
    let (fused, _) = run_log_test_with_fusion(
        &assembler,
        &effect,
        Arc::new(TestHost::with_wake_heap(assembler.core_values(), heap)),
        false,
    );
    assert_eq!(
        fusion_result_bytes(&assembler, fused),
        fusion_result_bytes(&assembler, unfused)
    );
}

#[test]
fn isolated_search_retains_branch_local_journals_without_committing() {
    let (assembler, effect) = compile_effect(
        ".alt ((.write_stderr \"left journal\") =>> .heap.set ['choice] \"left\" =>> .heap.get ['choice]) ((.write_stderr \"right journal\") =>> .heap.set ['choice] \"right\" =>> .heap.get ['choice])",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut search = IsolatedEffectSearch::new(
        &assembler.evaluation_runtime(),
        &effect,
        TestEffects,
        host.clone(),
    )
    .expect("isolated effect should initialize");
    let results = poll_isolated_search(&mut search);

    assert_eq!(results.len(), 2);
    assert_eq!(
        assembler
            .to_binary(results[0].value().expect("left branch should succeed"))
            .unwrap(),
        b"left".as_slice()
    );
    assert_eq!(
        assembler
            .to_binary(results[1].value().expect("right branch should succeed"))
            .unwrap(),
        b"right".as_slice()
    );
    assert_eq!(
        results[0].journal().stderr,
        [Bytes::from_static(b"left journal")]
    );
    assert_eq!(
        results[1].journal().stderr,
        [Bytes::from_static(b"right journal")]
    );
    assert!(host.stderr().is_empty());
    assert_eq!(host.heap(), assembler.values().empty_dict());
}

#[test]
fn isolated_search_retains_failed_branch_journals_as_parse_evidence() {
    let (assembler, effect) =
        compile_effect(".alt ((.write_stderr \"failed evidence\") =>> .fail) (.r \"success\")");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut search = IsolatedEffectSearch::new(
        &assembler.evaluation_runtime(),
        &effect,
        TestEffects,
        host.clone(),
    )
    .expect("isolated effect should initialize");
    let results = poll_isolated_search(&mut search);

    assert_eq!(results.len(), 2);
    assert!(results[0].value().is_none());
    assert_eq!(
        results[0].journal().stderr,
        [Bytes::from_static(b"failed evidence")]
    );
    assert!(results[1].value().is_some());
    assert!(host.stderr().is_empty());
}

#[test]
fn isolated_search_reports_and_resumes_retryable_state_observations() {
    let (assembler, effect) =
        compile_effect(".heap.get ['answer] >>= (\\answer -> (answer == \"ready\") =>> .r answer)");
    let host = Arc::new(TestHost::with_wake_heap(
        assembler.core_values(),
        public_record(&assembler, [("answer", assembler.values().text("ready"))]),
    ));
    let mut search = IsolatedEffectSearch::new(
        &assembler.evaluation_runtime(),
        &effect,
        TestEffects,
        host.clone(),
    )
    .expect("isolated effect should initialize");

    let generation = loop {
        match search.poll(256) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Blocked(blocked) => {
                assert!(!blocked.waiting_on_dependency());
                break blocked
                    .observed_generation()
                    .expect("retryable search must retain its observed generation");
            }
            IsolatedSearchPoll::Complete(_) => {
                panic!("search completed before its observed state changed")
            }
            IsolatedSearchPoll::Failed(error) => panic!("search failed: {error}"),
            IsolatedSearchPoll::Cancelled => panic!("search was cancelled"),
        }
    };
    assert!(host.wait_for_change(generation));

    let results = poll_isolated_search(&mut search);
    assert_eq!(results.len(), 1);
    assert_eq!(
        assembler
            .to_binary(results[0].value().expect("retry branch should succeed"))
            .unwrap(),
        b"ready".as_slice()
    );
    assert_eq!(host.wait_count(), 1);
}

#[test]
fn isolated_search_retries_observed_errors_without_advancing_choice() {
    let (assembler, effect) = compile_effect(".heap.get ['handler] >>= (\\handler -> handler ())");
    let (_, handler) =
        compile_effect_with_runtime(&assembler.evaluation_runtime(), "\\_ -> .r \"recovered\"");
    let host = Arc::new(TestHost::with_wake_heap(
        assembler.core_values(),
        public_record(&assembler, [("handler", handler)]),
    ));
    let mut search = IsolatedEffectSearch::new(
        &assembler.evaluation_runtime(),
        &effect,
        TestEffects,
        host.clone(),
    )
    .expect("isolated effect should initialize");

    let generation = loop {
        match search.poll(256) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Blocked(blocked) => {
                assert!(!blocked.waiting_on_dependency());
                assert!(blocked.error().is_some_and(|error| {
                    error.to_string().contains("requires a function value")
                }));
                break blocked
                    .observed_generation()
                    .expect("observed error must retain its generation");
            }
            IsolatedSearchPoll::Complete(_) => {
                panic!("search completed before its observed state changed")
            }
            IsolatedSearchPoll::Failed(error) => panic!("search failed: {error}"),
            IsolatedSearchPoll::Cancelled => panic!("search was cancelled"),
        }
    };
    assert!(host.wait_for_change(generation));

    let results = poll_isolated_search(&mut search);
    assert_eq!(results.len(), 1);
    assert_eq!(
        assembler
            .to_binary(results[0].value().expect("recovered branch should succeed"))
            .unwrap(),
        b"recovered".as_slice()
    );
}

#[test]
fn isolated_search_keeps_unobserved_errors_terminal() {
    let (assembler, effect) = compile_effect(".alt (1 2) (.r \"not reached\")");
    let host: Arc<dyn TaskHost<StandardEffects>> =
        Arc::new(TestHost::with_values(assembler.core_values()));
    let mut search = IsolatedEffectSearch::new(
        &assembler.evaluation_runtime(),
        &effect,
        StandardEffects,
        host,
    )
    .expect("isolated effect should initialize");

    loop {
        match search.poll(256) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Failed(error) => {
                assert!(error.to_string().contains("requires a function value"));
                break;
            }
            IsolatedSearchPoll::Blocked(_) => panic!("unobserved error should not block"),
            IsolatedSearchPoll::Complete(_) => {
                panic!("an evaluation error must not advance to another branch")
            }
            IsolatedSearchPoll::Cancelled => panic!("search was cancelled"),
        }
    }
}

#[test]
fn isolated_search_can_be_cancelled_between_polls() {
    let (assembler, effect) = compile_effect(".r \"unused\"");
    let host: Arc<dyn TaskHost<StandardEffects>> =
        Arc::new(TestHost::with_values(assembler.core_values()));
    let mut search = IsolatedEffectSearch::new(
        &assembler.evaluation_runtime(),
        &effect,
        StandardEffects,
        host,
    )
    .expect("isolated effect should initialize");
    search.cancel();
    assert!(matches!(search.poll(256), IsolatedSearchPoll::Cancelled));
}

#[test]
fn isolated_search_reports_and_resumes_lazy_dependencies() {
    let (assembler, function) =
        compile_effect("\\value -> .eval value >>= (\\result -> .r result.ok)");
    let session = EvalContext::isolated(assembler.core_values());
    let (promised, _owner_task, _owner) = session
        .task_owned_promise(Arc::from("isolated search dependency"))
        .unwrap();
    let observer = session.with_new_task().unwrap();
    let effect = eval::apply_values(
        &observer,
        function.as_core().clone(),
        vec![Value::Promised(promised.clone())],
    )
    .unwrap();
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut search = IsolatedEffectSearch::new_in_context(
        &PublicValue::from_core(&assembler.core_values(), effect),
        TestEffects,
        host,
        observer,
    )
    .expect("isolated effect should initialize");

    loop {
        match search.poll(256) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Blocked(blocked) => {
                assert!(blocked.waiting_on_dependency());
                assert!(blocked.observed_generation().is_none());
                assembler
                    .core_values()
                    .collect_managed_for_test()
                    .expect("blocked request decoding must retain no evaluator mutator");
                break;
            }
            IsolatedSearchPoll::Complete(_) => {
                panic!("search completed before its dependency")
            }
            IsolatedSearchPoll::Failed(error) => panic!("search failed: {error}"),
            IsolatedSearchPoll::Cancelled => panic!("search was cancelled"),
        }
    }
    promised
        .set(Value::Binary(Bytes::from_static(b"ready")))
        .expect("test dependency should resolve once");

    let results = poll_isolated_search(&mut search);
    assert_eq!(results.len(), 1);
    assert_eq!(
        assembler
            .to_binary(results[0].value().expect("resumed branch should succeed"))
            .unwrap(),
        b"ready".as_slice()
    );
}

#[test]
fn effect_task_poll_yields_and_resumes_with_bounded_fuel() {
    let (assembler, effect) =
        compile_effect(".r \"A\" >>= (\\a -> .r \"B\" >>= (\\b -> .r (a ++ b)))");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut task = EffectTask::new(
        &assembler.core_values(),
        effect.as_core().clone(),
        TestEffects,
        host,
    )
    .unwrap();

    assert!(matches!(task.poll(1), EffectTaskPoll::Yielded));
    let value = loop {
        match task.poll(1) {
            EffectTaskPoll::Yielded => {}
            EffectTaskPoll::Complete(value) => break value,
            EffectTaskPoll::Blocked(_) => panic!("finite task unexpectedly blocked"),
            EffectTaskPoll::Failed(error) => panic!("finite task failed: {error}"),
            EffectTaskPoll::Cancelled => panic!("finite task was cancelled"),
            EffectTaskPoll::Exit(_) => panic!("finite task unexpectedly voted to exit"),
        }
    };
    assert_eq!(assembler.to_binary(&value).unwrap(), b"AB".as_slice());
}

#[test]
#[should_panic(expected = "poll context and evaluator context must share one demand session")]
fn scheduled_effect_wrapper_rejects_an_unrelated_poll_context() {
    let (assembler, effect) = compile_effect(".r ()");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (task_context, _task_owner) = EvalContext::isolated(assembler.core_values()).into_parts();
    let unrelated = EvalContext::isolated(assembler.core_values());
    let poll_context = crate::evaluation::EvaluationPollContext::for_context(&unrelated);
    let task =
        EffectTask::new_in_context(effect.as_core().clone(), TestEffects, host, task_context)
            .expect("effect task should build");
    let mut machine = ValueEffectTask(task);

    let _ = machine.poll(&poll_context, 1);
}

#[test]
fn completed_effect_root_is_not_recreated_after_scope() {
    let source = include_str!("../machine.rs");
    assert_eq!(
        source
            .matches("EvaluationMachinePoll::Complete(value.into_runtime_root())")
            .count(),
        2,
        "value-returning and unit-returning scheduled effects must preserve their public roots"
    );
    assert!(
        !source.contains("root_value(value.into_core())"),
        "scheduled effect completion must not extract and recreate a root"
    );
}

fn poll_machine_exit(
    machine: &mut dyn EvaluationTaskMachine,
    poll_context: &crate::evaluation::EvaluationPollContext,
) -> EvaluationExitBlock {
    loop {
        match machine.poll(poll_context, 256) {
            EvaluationMachinePoll::Yielded => {}
            EvaluationMachinePoll::Exit(exit) => return exit,
            EvaluationMachinePoll::Blocked(_) => {
                panic!("exit fixture unexpectedly blocked")
            }
            EvaluationMachinePoll::Complete(_) => {
                panic!("exit fixture unexpectedly completed")
            }
            EvaluationMachinePoll::Failed(error) => {
                panic!("exit fixture failed: {error}")
            }
            EvaluationMachinePoll::Cancelled => {
                panic!("exit fixture was cancelled")
            }
        }
    }
}

#[test]
fn internal_exit_success_projects_through_both_scheduled_effect_wrappers() {
    let (assembler, effect) = compile_effect(".exit.success");

    for require_unit in [false, true] {
        let (context, owner) = EvalContext::isolated(assembler.core_values()).into_parts();
        let poll_context = crate::evaluation::EvaluationPollContext::for_context(&context);
        let task = EffectTask::new_exit_in_context(
            effect.as_core().clone(),
            TestEffects,
            Arc::new(TestHost::with_values(assembler.core_values())),
            context,
        )
        .expect("internal exit task should initialize");
        let mut machine: Box<dyn EvaluationTaskMachine> = if require_unit {
            Box::new(UnitEffectTask(task))
        } else {
            Box::new(ValueEffectTask(task))
        };

        let exit = poll_machine_exit(machine.as_mut(), &poll_context);
        assert_eq!(exit.intent, ExitIntent::Success);
        assert_eq!(exit.observed_epoch, None);
        assert!(matches!(
            machine.poll(&poll_context, 1),
            EvaluationMachinePoll::Exit(EvaluationExitBlock {
                intent: ExitIntent::Success,
                observed_epoch: None,
            })
        ));
        drop(owner);
    }
}

#[test]
fn internal_exit_error_forces_and_roots_its_message() {
    let (assembler, effect) =
        compile_effect(".exit.error ((\\message -> message) {msg:{text:\"stop\"}, detail:7})");
    let (context, owner) = EvalContext::isolated(assembler.core_values()).into_parts();
    let poll_context = crate::evaluation::EvaluationPollContext::for_context(&context);
    let task = EffectTask::new_exit_in_context(
        effect.as_core().clone(),
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
        context,
    )
    .expect("internal exit task should initialize");
    let mut machine = ValueEffectTask(task);

    let exit = poll_machine_exit(&mut machine, &poll_context);
    let ExitIntent::Error(message) = exit.intent else {
        panic!("error exit should retain its message")
    };
    assert_eq!(message.runtime_id(), assembler.core_values().runtime_id());
    assert!(matches!(message.as_core(), Value::Dict(_)));
    let message = PublicValue::from_core(&assembler.core_values(), message.into_core());
    assert_eq!(
        assembler
            .to_binary(&assembler.get(&message, "msg.text").unwrap())
            .unwrap(),
        b"stop".as_slice()
    );
    drop(owner);
}

#[test]
fn internal_exit_error_message_failure_is_an_ordinary_task_failure() {
    let (assembler, effect) =
        compile_effect(".exit.error (anno 'error {msg:{text:\"exit message failed\"}})");
    let (context, owner) = EvalContext::isolated(assembler.core_values()).into_parts();
    let mut task = EffectTask::new_exit_in_context(
        effect.as_core().clone(),
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
        context,
    )
    .expect("internal exit task should initialize");

    loop {
        match task.poll(256) {
            EffectTaskPoll::Yielded => {}
            EffectTaskPoll::Failed(_) => break,
            EffectTaskPoll::Blocked(_) => {
                panic!("failed exit message unexpectedly blocked")
            }
            EffectTaskPoll::Complete(_) => {
                panic!("failed exit message unexpectedly completed")
            }
            EffectTaskPoll::Cancelled => {
                panic!("failed exit message was cancelled")
            }
            EffectTaskPoll::Exit(_) => {
                panic!("failed exit message produced an exit vote")
            }
        }
    }
    drop(owner);
}

#[test]
fn permanent_exit_discards_every_speculative_cut_resource() {
    let (assembler, effect) = compile_effect(
        ".cut ((.heap.set ['discarded] 1) =>> (.write_stderr \"discarded\") =>> (.log 'error {msg:{text:\"discarded\"}}) =>> .task.new (.r \"child\") >>= (\\_child -> .alt (.fail) ((.exit.success) =>> .write_stderr \"after exit\")))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let owned = EvalContext::isolated(assembler.core_values());
    let builds = Arc::new(AtomicUsize::new(0));
    let launcher: Arc<dyn ReflectionTaskLauncher> = Arc::new(CountingLauncher {
        inner: task_launcher(TestEffects, host.clone()),
        builds: builds.clone(),
    });
    owned
        .install_reflection_launcher(launcher)
        .expect("fresh exit fixture should accept its task profile");
    let (context, owner) = owned.into_parts();
    let poll_context = crate::evaluation::EvaluationPollContext::for_context(&context);
    let observer = context.clone();
    let task = EffectTask::new_exit_in_context(
        effect.as_core().clone(),
        TestEffects,
        host.clone(),
        context,
    )
    .expect("internal exit task should initialize");
    let mut machine = ValueEffectTask(task);

    let exit = poll_machine_exit(&mut machine, &poll_context);
    assert_eq!(exit.intent, ExitIntent::Success);
    assert_eq!(exit.observed_epoch, None);
    assert!(
        assembler
            .get(&host.heap(), "discarded")
            .is_ok_and(|value| value.is_undefined())
    );
    assert!(host.diagnostics().is_empty());
    assert!(host.stderr().is_empty());
    assert_eq!(builds.load(Ordering::Acquire), 0);
    assert_eq!(
        observer.reflection_task_count(),
        0,
        "dropping every journal clone must retire its reserved child task"
    );
    drop(owner);
}

#[test]
fn retryable_exit_restarts_with_a_fresh_transaction_after_disturbance() {
    let (assembler, effect) = compile_effect(
        ".cut (.heap.get ['ready] >>= (\\ready -> (.heap.set ['attempt] \"committed\") =>> (.write_stderr \"once\") =>> (.log 'warn {msg:{text:\"once\"}}) =>> .alt ((ready == 1) =>> .r \"done\") (.exit.success)))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, owner) = EvalContext::isolated(assembler.core_values()).into_parts();
    let poll_context = crate::evaluation::EvaluationPollContext::for_context(&context);
    let task = EffectTask::new_exit_in_context(
        effect.as_core().clone(),
        TestEffects,
        host.clone(),
        context,
    )
    .expect("internal exit task should initialize");
    let mut machine = ValueEffectTask(task);

    let first = poll_machine_exit(&mut machine, &poll_context);
    assert_eq!(first.intent, ExitIntent::Success);
    assert!(first.observed_epoch.is_some());
    assert!(
        assembler
            .get(&host.heap(), "attempt")
            .is_ok_and(|value| value.is_undefined())
    );
    assert!(host.stderr().is_empty());
    assert!(host.diagnostics().is_empty());
    assert!(matches!(
        machine.poll(&poll_context, 256),
        EvaluationMachinePoll::Exit(EvaluationExitBlock {
            intent: ExitIntent::Success,
            observed_epoch: Some(_),
        })
    ));

    let (_, disturb) =
        compile_effect_with_runtime(&assembler.evaluation_runtime(), ".heap.set ['ready] 1");
    assert!(matches!(
        run_log_test(&assembler, &disturb, host.clone()).unwrap(),
        TaskOutcome::Complete(_)
    ));

    let value = loop {
        match machine.poll(&poll_context, 256) {
            EvaluationMachinePoll::Yielded => {}
            EvaluationMachinePoll::Complete(value) => break value,
            EvaluationMachinePoll::Blocked(_) => {
                panic!("disturbed exit retry unexpectedly blocked")
            }
            EvaluationMachinePoll::Exit(_) => {
                panic!("disturbed exit did not restart its transaction")
            }
            EvaluationMachinePoll::Failed(error) => {
                panic!("disturbed exit retry failed: {error}")
            }
            EvaluationMachinePoll::Cancelled => {
                panic!("disturbed exit retry was cancelled")
            }
        }
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(value))
            .unwrap(),
        b"done".as_slice()
    );
    assert!(assembler.get(&host.heap(), "attempt").is_ok());
    assert_eq!(host.stderr(), [Bytes::from_static(b"once")]);
    assert_eq!(host.diagnostics().len(), 1);
    drop(owner);
}

#[test]
fn direct_effect_profiles_do_not_expose_exit() {
    let (assembler, effect) = compile_effect(".exit.success");
    let normal = EffectTask::new(
        &assembler.core_values(),
        effect.as_core().clone(),
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
    )
    .expect("ordinary effect task should initialize");
    let Value::Dict(normal_api) = &normal.api else {
        panic!("effect API should be a dictionary")
    };
    assert!(normal_api.get(&Key::atom_from_text("exit")).is_none());

    let (context, owner) = EvalContext::isolated(assembler.core_values()).into_parts();
    let internal = EffectTask::new_exit_in_context(
        effect.as_core().clone(),
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
        context,
    )
    .expect("internal exit task should initialize");
    let Value::Dict(internal_api) = &internal.api else {
        panic!("effect API should be a dictionary")
    };
    assert!(internal_api.get(&Key::atom_from_text("exit")).is_some());
    assert!(run_standard_test(&assembler, &effect).is_err());
    assert!(
        EffectRun::new(
            &assembler.evaluation_runtime(),
            &effect,
            TestEffects,
            Arc::new(TestHost::with_values(assembler.core_values())),
        )
        .run()
        .is_err(),
        "direct synchronous EffectRun must not expose coordinator exit"
    );
    drop(owner);
}

#[test]
fn evaluation_session_pumps_a_type_erased_effect_task() {
    let (assembler, effect) = compile_effect("(.write_stderr \"scheduled\") =>> .r ()");
    let context = EvalContext::isolated(assembler.core_values());
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let launcher = task_launcher(TestEffects, host.clone());
    let task = context
        .schedule_task(|task_context| {
            launcher
                .build(
                    task_context,
                    effect.as_core().clone(),
                    ReflectionTaskResultPolicy::RequireUnit,
                )
                .map_err(|error| Arc::from(error.to_string()))
        })
        .expect("effect task should schedule");

    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Pending(_)
    ));
    assert_eq!(
        context.pump_wait(task.wait(), 1),
        crate::evaluation::EvaluationPumpOutcome::BudgetExhausted
    );
    assert_eq!(
        context.pump_wait(task.wait(), 4096),
        crate::evaluation::EvaluationPumpOutcome::TargetReady
    );
    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Complete(_)
    ));
    assert_eq!(host.stderr(), [Bytes::from_static(b"scheduled")]);
}

#[test]
fn reflection_task_launcher_returns_arbitrary_effect_result_when_requested() {
    let (assembler, effect) = compile_effect(".r 42");
    let context = EvalContext::isolated(assembler.core_values());
    let launcher = task_launcher(
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let task = context
        .schedule_task(|task_context| {
            launcher
                .build(
                    task_context,
                    effect.as_core().clone(),
                    ReflectionTaskResultPolicy::ReturnValue,
                )
                .map_err(|error| Arc::from(error.to_string()))
        })
        .expect("effect task should schedule");

    assert_eq!(
        context.pump_wait(task.wait(), 4096),
        crate::evaluation::EvaluationPumpOutcome::TargetReady
    );
    let EvaluationWaitPoll::Complete(value) = context.poll_reflection_task(&task) else {
        panic!("the result-returning task should complete")
    };
    assert_eq!(value.as_core(), &Value::Number(Number::from(42)));
}

#[test]
fn reflection_task_launcher_requires_unit_when_requested() {
    let (assembler, effect) = compile_effect(".r 42");
    let context = EvalContext::isolated(assembler.core_values());
    let launcher = task_launcher(
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let task = context
        .schedule_task(|task_context| {
            launcher
                .build(
                    task_context,
                    effect.as_core().clone(),
                    ReflectionTaskResultPolicy::RequireUnit,
                )
                .map_err(|error| Arc::from(error.to_string()))
        })
        .expect("effect task should schedule");

    assert_eq!(
        context.pump_wait(task.wait(), 4096),
        crate::evaluation::EvaluationPumpOutcome::TargetReady
    );
    let EvaluationWaitPoll::Failed(error) = context.poll_reflection_task(&task) else {
        panic!("the unit-requiring task should fail")
    };
    assert_eq!(
        error.to_string(),
        "reflection annotation result: unit expected, received Number"
    );
}

fn schedule_composed_test_task(
    assembler: &Assembler,
    effect: &PublicValue,
    host: Arc<TestHost>,
) -> (OwnedEvalContext, EvaluationTaskHandle) {
    let context = EvalContext::isolated(assembler.core_values());
    context
        .install_reflection_launcher(task_launcher(TestEffects, host.clone()))
        .expect("fresh test session should accept a reflection launcher");
    let effect = effect.as_core().clone();
    let task = context
        .schedule_task(move |task_context| {
            EffectTask::new_in_context(effect, TestEffects, host, task_context)
                .map(|task| Box::new(ValueEffectTask(task)) as Box<dyn EvaluationTaskMachine>)
                .map_err(|error| Arc::from(error.to_string()))
        })
        .expect("test task should schedule");
    (context, task)
}

fn schedule_exit_child_test_task(
    assembler: &Assembler,
    effect: &PublicValue,
    host: Arc<TestHost>,
) -> (OwnedEvalContext, EvaluationTaskHandle) {
    let context = EvalContext::isolated(assembler.core_values());
    context
        .install_reflection_launcher(Arc::new(ExitCapableLauncher { host: host.clone() }))
        .expect("fresh test session should accept an exit-capable launcher");
    let effect = effect.as_core().clone();
    let task = context
        .schedule_task(move |task_context| {
            EffectTask::new_in_context(effect, TestEffects, host, task_context)
                .map(|task| Box::new(ValueEffectTask(task)) as Box<dyn EvaluationTaskMachine>)
                .map_err(|error| Arc::from(error.to_string()))
        })
        .expect("test task should schedule");
    (context, task)
}

fn pump_composed_test_task(
    context: &EvalContext,
    task: &EvaluationTaskHandle,
) -> EvaluationWaitPoll {
    assert_eq!(
        context.pump_wait(task.wait(), 16_384),
        crate::evaluation::EvaluationPumpOutcome::TargetReady
    );
    context.poll_reflection_task(task)
}

#[test]
fn effect_run_composes_runtime_children_and_unit_policy() {
    let (assembler, effect) =
        compile_effect(".task.new (.log 'warn { msg:{ text:\"child\" } }) >>= (\\_task -> .r ())");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));

    assert!(matches!(
        EffectRun::new(
            &assembler.evaluation_runtime(),
            &effect,
            TestEffects,
            host.clone()
        )
        .requiring_unit_result()
        .run()
        .unwrap(),
        TaskOutcome::Complete(_)
    ));
    assert_eq!(host.diagnostics().len(), 1);
}

#[test]
fn scheduled_effect_root_completes_with_children_and_publishes_lifecycle() {
    let (assembler, effect) =
        compile_effect(".task.new (.log 'warn { msg:{ text:\"child\" } }) >>= (\\_task -> .r ())");
    let runtime = assembler.evaluation_runtime();
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let lifecycle = EffectLifecycle::new(&runtime);
    let task = EffectRun::new(&runtime, &effect, TestEffects, host.clone())
        .requiring_unit_result()
        .schedule(&lifecycle)
        .expect("scheduled root should be admitted");

    assert!(matches!(task.run().unwrap(), TaskOutcome::Complete(_)));
    assert!(matches!(
        lifecycle.status(),
        EffectLifecycleStatus::Complete(_)
    ));
    assert_eq!(host.diagnostics().len(), 1);
}

#[test]
fn scheduled_effect_root_publishes_failure_and_cancellation() {
    let (failure_assembler, failure_effect) = compile_effect(".fail");
    let failure_runtime = failure_assembler.evaluation_runtime();
    let failure_lifecycle = EffectLifecycle::new(&failure_runtime);
    let failed = EffectRun::new(
        &failure_runtime,
        &failure_effect,
        TestEffects,
        Arc::new(TestHost::with_values(failure_assembler.core_values())),
    )
    .schedule(&failure_lifecycle)
    .expect("failed root should first be admitted");
    assert!(failed.run().is_err());
    assert!(matches!(
        failure_lifecycle.status(),
        EffectLifecycleStatus::Failed(_)
    ));

    let (cancel_assembler, cancel_effect) = compile_effect(".read_log");
    let cancel_runtime = cancel_assembler.evaluation_runtime();
    let cancel_lifecycle = EffectLifecycle::new(&cancel_runtime);
    let cancelled = EffectRun::new(
        &cancel_runtime,
        &cancel_effect,
        TestEffects,
        Arc::new(TestHost::with_values(cancel_assembler.core_values())),
    )
    .schedule(&cancel_lifecycle)
    .expect("blocked root should be admitted");
    cancel_runtime.pump_until_stable();
    assert_eq!(cancel_lifecycle.status(), EffectLifecycleStatus::Blocked);
    cancelled.cancel();
    assert!(matches!(cancelled.run().unwrap(), TaskOutcome::Cancelled));
    assert_eq!(cancel_lifecycle.status(), EffectLifecycleStatus::Cancelled);
}

fn terminal_test_lifecycle(
    runtime: &EvaluationRuntime,
) -> (
    EffectLifecycle,
    Arc<Mutex<Vec<EvaluationTaskStatus>>>,
    Arc<AtomicBool>,
) {
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let published = statuses.clone();
    let notified = Arc::new(AtomicBool::new(false));
    let notified_after_release = notified.clone();
    let runtime_after_release = runtime.clone();
    let serial_runtime = runtime.worker_threads() == 0;
    let terminal = EffectLifecycleTerminal::new(
        runtime.id(),
        TaskStatusPublisher::new(move |_mutation, status| {
            published.lock().unwrap().push(status);
            let notified_after_release = notified_after_release.clone();
            let runtime_after_release = runtime_after_release.clone();
            TaskStatusWake::new(move || {
                if serial_runtime {
                    assert!(
                        runtime_after_release.exclusive_admission_available(),
                        "terminal wake must run after runtime mutation admission is released"
                    );
                }
                notified_after_release.store(true, Ordering::Release);
            })
        }),
    );
    (
        EffectLifecycle::new_with_terminal(runtime, terminal),
        statuses,
        notified,
    )
}

#[test]
fn coordinator_terminal_policy_observes_every_root_disposition() {
    fn recorded_status(statuses: &Arc<Mutex<Vec<EvaluationTaskStatus>>>) -> EvaluationTaskStatus {
        let statuses = statuses.lock().unwrap();
        assert_eq!(statuses.len(), 1);
        statuses[0].clone()
    }

    let (complete_assembler, complete_effect) = compile_effect(".r ()");
    let complete_runtime = complete_assembler.evaluation_runtime();
    let (complete_lifecycle, complete_statuses, complete_notified) =
        terminal_test_lifecycle(&complete_runtime);
    let complete = EffectRun::new(
        &complete_runtime,
        &complete_effect,
        TestEffects,
        Arc::new(TestHost::with_values(complete_assembler.core_values())),
    )
    .schedule(&complete_lifecycle)
    .unwrap();
    assert!(matches!(complete.run().unwrap(), TaskOutcome::Complete(_)));
    assert!(matches!(
        recorded_status(&complete_statuses),
        EvaluationTaskStatus::Complete(_)
    ));
    assert!(complete_notified.load(Ordering::Acquire));

    let (failed_assembler, failed_effect) = compile_effect(".fail");
    let failed_runtime = failed_assembler.evaluation_runtime();
    let (failed_lifecycle, failed_statuses, failed_notified) =
        terminal_test_lifecycle(&failed_runtime);
    let failed = EffectRun::new(
        &failed_runtime,
        &failed_effect,
        TestEffects,
        Arc::new(TestHost::with_values(failed_assembler.core_values())),
    )
    .schedule(&failed_lifecycle)
    .unwrap();
    assert!(failed.run().is_err());
    assert!(matches!(
        recorded_status(&failed_statuses),
        EvaluationTaskStatus::Failed(_)
    ));
    assert!(failed_notified.load(Ordering::Acquire));

    let (cancelled_assembler, cancelled_effect) = compile_effect(".read_log");
    let cancelled_runtime = cancelled_assembler.evaluation_runtime();
    let (cancelled_lifecycle, cancelled_statuses, cancelled_notified) =
        terminal_test_lifecycle(&cancelled_runtime);
    let cancelled = EffectRun::new(
        &cancelled_runtime,
        &cancelled_effect,
        TestEffects,
        Arc::new(TestHost::with_values(cancelled_assembler.core_values())),
    )
    .schedule(&cancelled_lifecycle)
    .unwrap();
    cancelled_runtime.pump_until_stable();
    cancelled.cancel();
    assert!(matches!(cancelled.run().unwrap(), TaskOutcome::Cancelled));
    assert_eq!(
        recorded_status(&cancelled_statuses),
        EvaluationTaskStatus::Cancelled
    );
    assert!(cancelled_notified.load(Ordering::Acquire));

    let (abandoned_assembler, abandoned_effect) = compile_effect(".read_log");
    let abandoned_runtime = abandoned_assembler.evaluation_runtime();
    let (abandoned_lifecycle, abandoned_statuses, abandoned_notified) =
        terminal_test_lifecycle(&abandoned_runtime);
    let abandoned = EffectRun::new(
        &abandoned_runtime,
        &abandoned_effect,
        TestEffects,
        Arc::new(TestHost::with_values(abandoned_assembler.core_values())),
    )
    .schedule(&abandoned_lifecycle)
    .unwrap();
    abandoned_runtime.pump_until_stable();
    drop(abandoned);
    assert_eq!(
        recorded_status(&abandoned_statuses),
        EvaluationTaskStatus::Abandoned
    );
    assert!(abandoned_notified.load(Ordering::Acquire));

    let (exited_assembler, exited_effect) = compile_effect(".exit.success");
    let exited_runtime = exited_assembler.evaluation_runtime();
    let (exited_lifecycle, exited_statuses, exited_notified) =
        terminal_test_lifecycle(&exited_runtime);
    let exited = EffectRun::new(
        &exited_runtime,
        &exited_effect,
        TestEffects,
        Arc::new(TestHost::with_values(exited_assembler.core_values())),
    )
    .schedule(&exited_lifecycle)
    .unwrap();
    exited_runtime.pump_until_stable();
    let crate::api::RuntimeReadiness::Ready(snapshot) = exited_runtime.readiness() else {
        panic!("exit-capable root should vote for a ready settlement")
    };
    snapshot.settle().expect("exit vote should settle");
    assert!(exited.run().is_err());
    assert_eq!(
        recorded_status(&exited_statuses),
        EvaluationTaskStatus::Exited
    );
    assert!(exited_notified.load(Ordering::Acquire));

    let (killed_assembler, killed_effect) = compile_effect(".read_log");
    let killed_runtime = killed_assembler.evaluation_runtime();
    let (killed_lifecycle, killed_statuses, killed_notified) =
        terminal_test_lifecycle(&killed_runtime);
    let killed = EffectRun::new(
        &killed_runtime,
        &killed_effect,
        TestEffects,
        Arc::new(TestHost::with_values(killed_assembler.core_values())),
    )
    .schedule(&killed_lifecycle)
    .unwrap();
    killed_runtime.pump_until_stable();
    let crate::api::RuntimeReadiness::Deadlocked(snapshot) = killed_runtime.readiness() else {
        panic!("blocked root should produce a deadlock snapshot")
    };
    snapshot
        .kill(crate::api::RuntimeKillReason::Deadlock)
        .settle()
        .expect("forced deadlock settlement should succeed");
    assert!(killed.run().is_err());
    assert!(matches!(
        recorded_status(&killed_statuses),
        EvaluationTaskStatus::Killed(_)
    ));
    assert!(killed_notified.load(Ordering::Acquire));
}

#[test]
fn coordinator_terminal_policy_closes_root_descendants_after_publication() {
    let (assembler, effect) = compile_effect(".task.new (.read_log) >>= (\\_child -> .r ())");
    let runtime = assembler.evaluation_runtime();
    let (lifecycle, statuses, notified) = terminal_test_lifecycle(&runtime);
    let task = EffectRun::new(
        &runtime,
        &effect,
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
    )
    .schedule(&lifecycle)
    .expect("root with a blocked child should schedule");

    assert!(matches!(task.run().unwrap(), TaskOutcome::Complete(_)));
    assert!(matches!(
        statuses.lock().unwrap().as_slice(),
        [EvaluationTaskStatus::Complete(_)]
    ));
    assert!(notified.load(Ordering::Acquire));
    runtime.pump_until_stable();
    let crate::api::RuntimeReadiness::Ready(snapshot) = runtime.readiness() else {
        panic!("closing the logger demand session should abandon its blocked child")
    };
    assert!(snapshot.dispositions().is_empty());
}

#[test]
fn coordinator_terminal_policy_preserves_a_descendant_failure_before_root_return() {
    for workers in [0, 4] {
        let runtime = EvaluationRuntime::new(workers).expect("test runtime should build");
        let (assembler, effect) = compile_effect_with_runtime(
            &runtime,
            ".task.new (.fail) >>= (\\_child -> .read_log >>= (\\_message -> .r ()))",
        );
        let (lifecycle, statuses, notified) = terminal_test_lifecycle(&runtime);
        let host = Arc::new(TestHost::with_values(assembler.core_values()));
        let task = EffectRun::new(&runtime, &effect, TestEffects, host.clone())
            .schedule(&lifecycle)
            .expect("root with a failing child should schedule");

        runtime.pump_until_stable();
        let mut status = lifecycle.status();
        while status != EffectLifecycleStatus::Blocked {
            assert!(
                !status.is_terminal(),
                "the held root must block before terminalizing: {status:?}"
            );
            status = lifecycle.wait_for_change(&status);
        }
        runtime.pump_until_stable();
        host.emit_diagnostic(Diagnostic::new(
            &assembler.values(),
            Severity::Info,
            "release logger root",
        ));

        let failure = task
            .run()
            .expect_err("a child failure which precedes root return remains authoritative");
        assert!(
            failure
                .to_string()
                .contains("reflection task failed permanently")
        );
        assert!(matches!(
            statuses.lock().unwrap().as_slice(),
            [EvaluationTaskStatus::Complete(_)]
        ));
        let snapshot = loop {
            runtime.pump_until_stable();
            match runtime.readiness() {
                crate::api::RuntimeReadiness::Busy => continue,
                crate::api::RuntimeReadiness::Ready(snapshot) => break snapshot,
                readiness @ crate::api::RuntimeReadiness::Deadlocked(_) => panic!(
                    "a terminal logger root and retained child failure should be ready, got {readiness:?}"
                ),
            }
        };
        assert!(
            notified.load(Ordering::Acquire),
            "terminal notification must finish before the runtime becomes stable"
        );
        assert!(snapshot.dispositions().is_empty());
        let report = snapshot
            .settle()
            .expect("terminal logger closure should settle without disturbance");
        assert_eq!(report.task_failures().len(), 1);
    }
}

#[test]
fn scheduled_effect_handle_retains_root_after_lifecycle_observer_drops() {
    let (assembler, effect) = compile_effect(".r 42");
    let runtime = assembler.evaluation_runtime();
    let lifecycle = EffectLifecycle::new(&runtime);
    let task = EffectRun::new(
        &runtime,
        &effect,
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
    )
    .schedule(&lifecycle)
    .expect("scheduled root should be admitted");
    drop(lifecycle);

    let TaskOutcome::Complete(value) = task.run().unwrap() else {
        panic!("hidden task handle should retain the coordinator root")
    };
    assert_eq!(value.as_i64(), Some(42));
}

#[test]
fn dropping_blocked_scheduled_effect_releases_its_session_and_publishes_abandonment() {
    let (assembler, effect) = compile_effect(".read_log");
    let runtime = assembler.evaluation_runtime();
    let lifecycle = EffectLifecycle::new(&runtime);
    let task = EffectRun::new(
        &runtime,
        &effect,
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
    )
    .schedule(&lifecycle)
    .expect("blocked root should be admitted");
    runtime.pump_until_stable();
    assert_eq!(lifecycle.status(), EffectLifecycleStatus::Blocked);

    drop(task);

    assert_eq!(lifecycle.status(), EffectLifecycleStatus::Abandoned);
}

#[test]
fn annotations_use_one_runtime_default_profile_across_demand_sessions() {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let captured = diagnostics.clone();
    let assembler = Assembler::builder()
        .diagnostic_callback(move |event| {
            captured.lock().unwrap().push(event.diagnostic().clone());
        })
        .build()
        .expect("assembler should seal its runtime reflection profile");
    let module = assembler
            .module(["runtime_default_profile"])
            .script(
                "g",
                "language g0\nimport 'std\nfirst = anno refl:(.log 'warn {msg:{text:\"first\"}}) (.r ())\nsecond = anno refl:(.log 'warn {msg:{text:\"second\"}}) (.r ())\n",
            )
            .build()
            .expect("annotation profile fixture should compile");
    diagnostics.lock().unwrap().clear();

    for name in ["first", "second"] {
        let effect = assembler
            .get(module.value(), name)
            .expect("fixture should define both effects");
        let claim_host = Arc::new(TestHost::with_values(assembler.core_values()));
        assert!(matches!(
            EffectRun::new(
                &assembler.evaluation_runtime(),
                &effect,
                TestEffects,
                claim_host.clone(),
            )
            .run()
            .unwrap(),
            TaskOutcome::Complete(_)
        ));
        assert!(
            claim_host.diagnostics().is_empty(),
            "annotation diagnostics must not use the claim-site profile"
        );
    }

    assert_eq!(
        diagnostics.lock().unwrap().len(),
        2,
        "both demand sessions must use the runtime's sealed default profile"
    );
}

#[test]
fn effect_run_separates_provider_assertions_from_its_generic_unit_policy() {
    let (assembler, effect) = compile_effect(".r 42");

    let generic = EffectRun::new(
        &assembler.evaluation_runtime(),
        &effect,
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
    )
    .requiring_unit_result()
    .run()
    .expect_err("the generic endpoint must reject non-unit results");
    assert_eq!(
        generic.to_string(),
        "effect task returned Number; expected unit"
    );

    let contextual = EffectRun::new(
        &assembler.evaluation_runtime(),
        &effect,
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
    )
    .asserting_unit_result("test task result")
    .requiring_unit_result()
    .run()
    .expect_err("the provider assertion must reject non-unit results first");
    assert_eq!(
        contextual.to_string(),
        "test task result: unit expected, received Number"
    );
}

#[test]
fn reflection_task_returns_a_joinable_result() {
    let (assembler, effect) =
        compile_effect(".task.new (.r \"child\") >>= (\\task -> .task.join task)");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host);
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("joined task should complete")
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*value))
            .unwrap(),
        b"child".as_slice()
    );
}

#[test]
fn dictionary_items_are_available_to_reflection_in_key_order() {
    let (assembler, effect) = compile_effect(".dict_items { b:2, a:1 }");
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("dict_items task should complete");
    };
    let value = value.into_core();
    let Value::List(items) = value else {
        panic!("dict_items should return a list");
    };
    let items = eval::list_to_value_items(&assembler.eval_context(), &items).unwrap();
    assert_eq!(items.len(), 2);
    let keys = items
        .into_iter()
        .map(|item| {
            let Value::Dict(item) = item else {
                panic!("dict_items entries should be records");
            };
            item.get(&*keys::KEY)
                .cloned()
                .expect("dict_items entries should include their key")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            Value::Atom(Atom::from_key(&Key::binary_from_text("a"))),
            Value::Atom(Atom::from_key(&Key::binary_from_text("b"))),
        ]
    );
}

#[test]
fn metadata_inspection_returns_hidden_values_without_forcing_them() {
    let (assembler, initial) = compile_effect(".meta.inspect (anno 'meta_init ())");
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &initial,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Complete(metadata) = pump_composed_test_task(&context, &task) else {
        panic!("metadata inspection should return the initial hidden dictionary");
    };
    let Value::Dict(metadata) = metadata.into_core() else {
        panic!("metadata inspection should return the initial hidden dictionary");
    };
    assert!(metadata.is_empty());

    let (assembler, inspect) = compile_effect("\\value -> .meta.inspect value");
    let carrier = PublicValue::from_core(
        &assembler.core_values(),
        Value::metadata_carrier(Value::error(
            &crate::core::test_value_factory(),
            "latent metadata failure",
        )),
    );
    let effect = assembler
        .apply(&inspect, [carrier])
        .expect("metadata inspection function should accept its carrier");
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Complete(metadata) = pump_composed_test_task(&context, &task) else {
        panic!("metadata inspection must not demand its hidden value");
    };
    assert!(matches!(metadata.as_core(), Value::Lazy(_)));
    let error = assembler
        .evaluate(&PublicValue::from_runtime_root(*metadata))
        .expect_err("the returned hidden failure should remain demandable");
    assert_eq!(error.to_string(), "latent metadata failure");
}

#[test]
fn effectful_metadata_update_observes_environment_and_commits_log_once() {
    let (assembler, effect) = compile_effect(&format!(
        ".meta.inspect (list.at 0 (anno meta_refl:(\\_priors -> .env ['glam,'version] >>= (\\version -> .log 'info {{msg:{{text:\"metadata update ran\"}}}} =>> .r [version])) [anno 'meta_init ()])) >>= (\\metadata -> (metadata == \"{}\") =>> .r metadata)",
        env!("CARGO_PKG_VERSION")
    ));
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host.clone());
    let EvaluationWaitPoll::Complete(metadata) = pump_composed_test_task(&context, &task) else {
        panic!("demanded effectful metadata should complete");
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*metadata))
            .unwrap(),
        env!("CARGO_PKG_VERSION").as_bytes()
    );
    assert_eq!(
        host.diagnostics().len(),
        1,
        "the shared metadata task must commit its diagnostic exactly once"
    );
}

#[test]
fn effectful_metadata_update_retries_after_observed_state_changes() {
    let (assembler, effect) = compile_effect(
        ".meta.inspect (list.at 0 (anno meta_refl:(\\_priors -> .heap.get ['ready] >>= (\\ready -> (ready == \"arrived for metadata\") =>> .r [ready])) [anno 'meta_init ()])) >>= (\\metadata -> (metadata == \"arrived for metadata\") =>> .r metadata)",
    );
    let host = Arc::new(TestHost::with_wake_heap(
        assembler.core_values(),
        public_record(
            &assembler,
            [("ready", assembler.values().text("arrived for metadata"))],
        ),
    ));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host.clone());
    assert_eq!(
        context.pump_wait(task.wait(), 16_384),
        crate::evaluation::EvaluationPumpOutcome::NoProgress,
        "the metadata task should first block on its observed heap state"
    );
    assert!(host.wait_for_change(1));
    let EvaluationWaitPoll::Complete(metadata) = pump_composed_test_task(&context, &task) else {
        panic!("the resumed metadata task should complete");
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*metadata))
            .unwrap(),
        b"arrived for metadata".as_slice()
    );
    assert_eq!(host.wait_count(), 1);
}

#[test]
fn metadata_inspection_mismatches_are_unobserved_effect_failures() {
    for ordinary in ["()", "42"] {
        let (assembler, effect) = compile_effect(&format!(
            ".cut (.alt (.meta.inspect {ordinary}) (.r \"fallback\"))"
        ));
        let (context, task) = schedule_composed_test_task(
            &assembler,
            &effect,
            Arc::new(TestHost::with_values(assembler.core_values())),
        );
        let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("metadata mismatch should permit an ordinary fallback");
        };
        assert_eq!(
            assembler
                .to_binary(&PublicValue::from_runtime_root(*value))
                .unwrap(),
            b"fallback".as_slice()
        );
    }

    let (assembler, effect) = compile_effect(".cut (.alt (.meta.inspect 42) (1 2))");
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Failed(error) = pump_composed_test_task(&context, &task) else {
        panic!("metadata mismatch must not make a later evaluator error retryable");
    };
    assert!(error.to_string().contains("requires a function value"));
}

#[test]
fn reflection_eval_returns_a_tagged_whnf_result() {
    let (assembler, effect) = compile_effect(".eval (1 + 2)");
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Complete(result) = pump_composed_test_task(&context, &task) else {
        panic!("eval should return an ok result");
    };
    let Value::Dict(result) = result.into_core() else {
        panic!("eval should return an ok result");
    };
    assert_eq!(
        result.get(&*keys::OK),
        Some(&Value::Number(Number::integer(3)))
    );

    let (_, nested) = compile_effect(".eval { bad:1 / 0 }");
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &nested,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Complete(result) = pump_composed_test_task(&context, &task) else {
        panic!("eval should return a tagged dictionary");
    };
    let Value::Dict(result) = result.into_core() else {
        panic!("eval should return a tagged dictionary");
    };
    let Some(Value::Dict(payload)) = result.get(&*keys::OK) else {
        panic!("eval should not recursively force a dictionary payload");
    };
    assert!(matches!(
        payload.get(&Key::atom_from_text("bad")),
        Some(Value::Lazy(_))
    ));
}

#[test]
fn reflection_eval_returns_evaluator_errors_as_data() {
    let (assembler, effect) = compile_effect(".eval (1 / 0) >>= (\\result -> .r result.err)");
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Complete(error) = pump_composed_test_task(&context, &task) else {
        panic!("eval should contain an evaluator error instead of failing its task");
    };
    let error = PublicValue::from_runtime_root(*error);
    let error = assembler
        .to_binary(
            &assembler
                .get(&error, "msg.text")
                .expect("eval error should have a diagnostic text view"),
        )
        .expect("eval error diagnostic text should be binary");
    assert!(String::from_utf8_lossy(&error).contains("zero"));
}

#[test]
fn reflection_eval_retries_terminal_lazy_dependencies() {
    let (assembler, success) =
        compile_effect(".eval (anno { refl:(.r ()) } \"ready\") >>= (\\result -> .r result.ok)");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &success, host.clone());
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("eval should resume after a successful lazy dependency");
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*value))
            .unwrap(),
        b"ready".as_slice()
    );

    let (_, failure) = compile_effect(
        ".eval (anno { refl:.fail } \"unreachable\") >>= (\\result -> .r result.err)",
    );
    let (context, task) = schedule_composed_test_task(&assembler, &failure, host);
    let EvaluationWaitPoll::Complete(error) = pump_composed_test_task(&context, &task) else {
        panic!("eval should convert a failed lazy dependency to err");
    };
    let error = PublicValue::from_runtime_root(*error);
    let error = assembler
        .to_binary(
            &assembler
                .get(&error, "msg.text")
                .expect("eval error should have a diagnostic text view"),
        )
        .expect("the eval error should remain observable after its producer fails");
    assert!(String::from_utf8_lossy(&error).contains("failed permanently"));
}

#[test]
fn reflection_eval_suspends_instead_of_failing_around_a_pending_value() {
    let (assembler, function) = compile_effect("\\value -> .eval value");
    let session = EvalContext::isolated(assembler.core_values());
    let (promised, _owner_task, _owner) = session
        .task_owned_promise(Arc::from("eval test dependency"))
        .unwrap();
    let observer = session.with_new_task().unwrap();
    let effect = eval::apply_values(
        &observer,
        function.as_core().clone(),
        vec![Value::Promised(promised.clone())],
    )
    .unwrap();
    let mut task = EffectTask::new_in_context(
        effect,
        TestEffects,
        Arc::new(TestHost::with_values(assembler.core_values())),
        observer,
    )
    .unwrap();

    let EffectTaskPoll::Blocked(blocked) = task.poll(256) else {
        panic!("eval should suspend on its value's pending dependency");
    };
    assert!(blocked.lazy.is_some());

    promised
        .fail_message("dependency failed")
        .expect("test promise should fail once");
    let poll = task.poll(256);
    let EffectTaskPoll::Complete(value) = poll else {
        panic!("eval should retry a terminal dependency and return err");
    };
    let Value::Dict(result) = value.into_core() else {
        panic!("eval should return a tagged result");
    };
    let Some(error) = result.get(&*keys::ERR) else {
        panic!("eval should return the dependency failure under err");
    };
    let error = PublicValue::from_core(&assembler.core_values(), error.clone());
    assert_eq!(
        assembler
            .to_binary(&assembler.get(&error, "msg.text").unwrap())
            .unwrap(),
        b"dependency failed".as_slice()
    );
}

#[test]
fn effect_map_runs_left_to_right_and_preserves_result_order() {
    let (assembler, effect) = compile_effect("eff.map (\\item -> .r item) [\"A\",\"B\",\"C\"]");
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("effect map task should complete");
    };
    let value = value.into_core();
    let Value::List(items) = value else {
        panic!("effect map should return a list");
    };
    let items = eval::list_to_value_items(&context, &items)
        .unwrap()
        .into_iter()
        .map(|mut item| {
            loop {
                item = eval::eval_value(&context, &item).unwrap();
                if !matches!(item, Value::Lazy(_) | Value::Promised(_)) {
                    break item;
                }
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        items,
        [
            Value::binary_from_text("A"),
            Value::binary_from_text("B"),
            Value::binary_from_text("C")
        ]
    );
}

#[test]
fn reflection_environment_is_available_as_plain_data() {
    let (assembler, version) = compile_effect(".env ['glam,'version]");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &version, host.clone());
    let EvaluationWaitPoll::Complete(version) = pump_composed_test_task(&context, &task) else {
        panic!("environment version should complete")
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*version))
            .unwrap(),
        env!("CARGO_PKG_VERSION").as_bytes()
    );

    let (_, environment) = compile_effect(
        ".env ['process,'env] >>= (\\environment -> (environment.[\"GLAM_TEST_ENV\"] == \"present\") =>> .r \"environment\")",
    );
    let (context, task) = schedule_composed_test_task(&assembler, &environment, host.clone());
    let environment_poll = pump_composed_test_task(&context, &task);
    assert!(
        matches!(environment_poll, EvaluationWaitPoll::Complete(_)),
        "process environment lookup should complete, got {environment_poll:?}"
    );

    let (_, arguments) = compile_effect(".env ['process,'args]");
    let (context, task) = schedule_composed_test_task(&assembler, &arguments, host.clone());
    let poll = pump_composed_test_task(&context, &task);
    let EvaluationWaitPoll::Complete(arguments) = poll else {
        panic!("process arguments should return a list, got {poll:?}")
    };
    let Value::List(arguments) = arguments.into_core() else {
        panic!("process arguments should return a list")
    };
    assert_eq!(
        eval::list_to_value_items(&context, &arguments).unwrap(),
        [
            Value::binary_from_text("glam"),
            Value::binary_from_text("--test")
        ]
    );

    let (_, child_environment) =
        compile_effect(".task.new (.env ['process,'args]) >>= (\\task -> .task.join task)");
    let (context, task) = schedule_composed_test_task(&assembler, &child_environment, host.clone());
    let EvaluationWaitPoll::Complete(arguments) = pump_composed_test_task(&context, &task) else {
        panic!("child reflection task should inherit its parent profile environment")
    };
    let Value::List(arguments) = arguments.into_core() else {
        panic!("child reflection task should inherit a list environment")
    };
    assert_eq!(
        eval::list_to_value_items(&context, &arguments).unwrap(),
        [
            Value::binary_from_text("glam"),
            Value::binary_from_text("--test")
        ]
    );

    let (_, missing) = compile_effect(
        ".env ['process,'env] >>= (\\environment -> (environment.[\"GLAM_TEST_MISSING\"] == {}) =>> .r \"missing\")",
    );
    let (context, task) = schedule_composed_test_task(&assembler, &missing, host);
    assert!(matches!(
        pump_composed_test_task(&context, &task),
        EvaluationWaitPoll::Complete(_)
    ));
}

#[test]
fn task_value_is_symmetric_with_task_error() {
    let (assembler, effect) = compile_effect(
        ".task.new (.r \"result\") >>= (\\task -> .task.join task >>= (\\_value -> .task.value task))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host);
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("task.value should return a completed task result")
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*value))
            .unwrap(),
        b"result".as_slice()
    );
}

#[test]
fn task_join_remains_available_after_the_child_is_terminal() {
    let (assembler, effect) = compile_effect(
        ".task.new (.r \"result\") >>= (\\task -> .task.join task >>= (\\_ -> .task.join task))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host);
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("a late join should retain the completed child result")
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*value))
            .unwrap(),
        b"result".as_slice()
    );
}

#[test]
fn task_status_reports_launched_and_terminal_states() {
    for source in [
        ".task.new (.r ()) >>= (\\task -> .task.status task >>= (\\status -> (status == 'launched) =>> .r ()))",
        ".task.new (.r ()) >>= (\\task -> .task.join task >>= (\\_ -> .task.status task >>= (\\status -> .r status.ok)))",
        ".task.new (.fail) >>= (\\task -> .task.error task >>= (\\_ -> .task.status task >>= (\\status -> .r status.err)))",
        ".task.new (.r ()) >>= (\\task -> .task.cancel task =>> .task.error task >>= (\\_ -> .task.status task >>= (\\status -> (status == 'canceled) =>> .r ())))",
        ".task.new (.r ()) >>= (\\task -> .task.join task >>= (\\_ -> .task.ack_error task =>> .task.status task >>= (\\status -> .r status.ok)))",
        ".task.new (.r ()) >>= (\\task -> .task.cancel task =>> .task.ack_error task =>> .task.ack_error task =>> .task.status task >>= (\\status -> (status == 'canceled) =>> .r ()))",
    ] {
        let (assembler, effect) = compile_effect(source);
        let host = Arc::new(TestHost::with_values(assembler.core_values()));
        let (context, task) = schedule_composed_test_task(&assembler, &effect, host.clone());
        let poll = pump_composed_test_task(&context, &task);
        assert!(
            matches!(poll, EvaluationWaitPoll::Complete(_)),
            "task status should match for {source}, got {poll:?}"
        );
    }
}

#[test]
fn task_observers_accept_handles_from_another_same_runtime_session() {
    let (assembler, publish) = compile_effect(
        ".task.new (.cut (.heap.get ['never] >>= (\\_ -> .fail))) >>= (\\blocked -> .task.new (.r \"done\") >>= (\\complete -> .task.new (.fail) >>= (\\failed -> .task.new (.r ()) >>= (\\canceled -> .task.cancel canceled =>> .heap.set ['observed_tasks] { blocked:blocked, complete:complete, failed:failed, canceled:canceled }))))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (owner, publish_task) = schedule_composed_test_task(&assembler, &publish, host.clone());
    assert!(matches!(
        pump_composed_test_task(&owner, &publish_task),
        EvaluationWaitPoll::Complete(_)
    ));
    let (_, inspect_launched) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        ".heap.get ['observed_tasks] >>= (\\tasks -> .task.status tasks.complete)",
    );
    let (launched_observer, inspect_launched_task) =
        schedule_composed_test_task(&assembler, &inspect_launched, host.clone());
    let EvaluationWaitPoll::Complete(launched) =
        pump_composed_test_task(&launched_observer, &inspect_launched_task)
    else {
        panic!("a same-runtime observer should read the pre-pump task status")
    };
    assert_eq!(
        launched.as_core(),
        &assembler.core_values().key_value(&keys::LAUNCHED)
    );
    drop(launched_observer);

    let EvaluationSessionRun::Deadlocked(before_observation) = owner.run_until_quiescent() else {
        panic!("the blocked child should leave its owner session deadlocked")
    };
    assert_eq!(before_observation.failures.size(), 1);
    assert_eq!(before_observation.unfinished.len(), 1);

    let (_, inspect) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        ".heap.get ['observed_tasks] >>= (\\tasks -> .task.status tasks.blocked >>= (\\blocked_status -> .task.status tasks.complete >>= (\\complete_status -> .task.value tasks.complete >>= (\\complete_value -> .task.status tasks.failed >>= (\\failed_status -> .task.error tasks.failed >>= (\\failed_error -> .task.status tasks.canceled >>= (\\canceled_status -> .r { blocked_status:blocked_status, complete_status:complete_status, complete_value:complete_value, failed_status:failed_status, failed_error:failed_error, canceled_status:canceled_status })))))))",
    );
    let (observer, inspect_task) = schedule_composed_test_task(&assembler, &inspect, host.clone());
    let EvaluationWaitPoll::Complete(observed) = pump_composed_test_task(&observer, &inspect_task)
    else {
        panic!("same-runtime task observations should complete")
    };
    let observed = eval::eval_value(&observer, observed.as_core())
        .expect("task observation result should evaluate");
    let Value::Dict(observed) = observed else {
        panic!("task observation fixture should return a dictionary")
    };
    let field = |name: &str| {
        eval::eval_value(
            &observer,
            observed
                .get(&Key::atom_from_text(name))
                .unwrap_or_else(|| panic!("task observation should define {name}")),
        )
        .unwrap_or_else(|error| panic!("task observation {name} should evaluate: {error}"))
    };
    assert_eq!(
        field("blocked_status"),
        assembler.core_values().key_value(&keys::BLOCKED)
    );
    let Value::Dict(complete_status) = field("complete_status") else {
        panic!("complete task status should be tagged data")
    };
    assert_eq!(
        eval::eval_value(
            &observer,
            complete_status
                .get(&*keys::OK)
                .expect("complete status should contain ok"),
        )
        .expect("complete status payload should evaluate"),
        Value::binary_from_text("done")
    );
    assert_eq!(field("complete_value"), Value::binary_from_text("done"));
    let Value::Dict(failed_status) = field("failed_status") else {
        panic!("failed task status should be tagged data")
    };
    assert!(failed_status.get(&*keys::ERR).is_some());
    assert!(matches!(field("failed_error"), Value::Dict(_)));
    assert_eq!(
        field("canceled_status"),
        assembler.core_values().key_value(&keys::CANCELED)
    );

    let EvaluationSessionRun::Deadlocked(after_observation) = owner.run_until_quiescent() else {
        panic!("observation must not disturb the blocked owner task")
    };
    assert_eq!(after_observation.failures, before_observation.failures);
    assert_eq!(after_observation.unfinished.len(), 1);

    drop(owner);
    let (_, inspect_abandoned) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        ".heap.get ['observed_tasks] >>= (\\tasks -> .task.status tasks.blocked)",
    );
    let (late_observer, inspect_abandoned_task) =
        schedule_composed_test_task(&assembler, &inspect_abandoned, host.clone());
    let EvaluationWaitPoll::Complete(abandoned) =
        pump_composed_test_task(&late_observer, &inspect_abandoned_task)
    else {
        panic!("a retained same-runtime handle should remain observable after owner closure")
    };
    assert_eq!(
        abandoned.as_core(),
        &assembler.core_values().key_value(&keys::ABANDONED),
        "observer-held task handles must not keep their producer demand open"
    );
}

#[test]
fn task_acknowledgement_routes_across_sessions_to_the_producer_ledger() {
    let (assembler, spawn) = compile_effect(".task.new (.fail)");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (owner, first_task) = schedule_composed_test_task(&assembler, &spawn, host.clone());
    let EvaluationWaitPoll::Complete(handle) = pump_composed_test_task(&owner, &first_task) else {
        panic!("first session should return a task handle")
    };
    let EvaluationSessionRun::Complete(report) = owner.run_until_quiescent() else {
        panic!("the producer should drain its failed child")
    };
    assert_eq!(report.failures.size(), 1);

    let (_, acknowledge) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        "\\task -> .cut (.task.ack_error task)",
    );
    let acknowledge = assembler
        .apply(&acknowledge, [PublicValue::from_runtime_root(*handle)])
        .expect("non-owner task acknowledgement should apply");
    let (second_context, second_task) = schedule_composed_test_task(&assembler, &acknowledge, host);
    assert!(matches!(
        pump_composed_test_task(&second_context, &second_task),
        EvaluationWaitPoll::Complete(_)
    ));
    assert_eq!(
        owner.task_registry_counts().unacknowledged_failures,
        0,
        "cross-session acknowledgement must remove the producer's failure entry"
    );
    assert_eq!(
        second_context
            .task_registry_counts()
            .unacknowledged_failures,
        0,
        "acknowledgement must not create or remove an observer-ledger entry"
    );
}

#[test]
fn task_join_accepts_same_runtime_handles_across_all_terminal_states() {
    let (assembler, publish) = compile_effect(
        ".task.new (.cut (.heap.get ['join_ready] >>= (\\ready -> (ready == \"ready\") =>> .r \"pending result\"))) >>= (\\pending -> .task.new (.cut (.heap.get ['never] >>= (\\_ -> .fail))) >>= (\\abandoned -> .task.new (.r \"complete result\") >>= (\\complete -> .task.new (.fail) >>= (\\failed -> .task.new (.r ()) >>= (\\canceled -> .task.cancel canceled =>> .heap.set ['join_tasks] { pending:pending, abandoned:abandoned, complete:complete, failed:failed, canceled:canceled })))))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (owner, publish_task) = schedule_composed_test_task(&assembler, &publish, host.clone());
    assert!(matches!(
        pump_composed_test_task(&owner, &publish_task),
        EvaluationWaitPoll::Complete(_)
    ));

    let EvaluationSessionRun::Deadlocked(initial_owner_report) = owner.run_until_quiescent() else {
        panic!("the two blocked children should leave their owner session deadlocked")
    };
    assert_eq!(initial_owner_report.failures.size(), 1);
    assert_eq!(initial_owner_report.unfinished.len(), 2);

    let (_, read_handles) =
        compile_effect_with_runtime(&assembler.evaluation_runtime(), ".heap.get ['join_tasks]");
    let (handle_reader, read_handles_task) =
        schedule_composed_test_task(&assembler, &read_handles, host.clone());
    let EvaluationWaitPoll::Complete(handles) =
        pump_composed_test_task(&handle_reader, &read_handles_task)
    else {
        panic!("a same-runtime observer should read the published task handles")
    };
    let Value::Dict(handles) = eval::eval_value(&handle_reader, handles.as_core())
        .expect("the published task-handle dictionary should evaluate")
    else {
        panic!("the task-handle fixture should publish a dictionary")
    };
    let handle = |name: &str| {
        eval::eval_value(
            &handle_reader,
            handles
                .get(&Key::atom_from_text(name))
                .unwrap_or_else(|| panic!("task-handle fixture should define {name}")),
        )
        .unwrap_or_else(|error| panic!("task handle {name} should evaluate: {error}"))
    };
    let (_, join) =
        compile_effect_with_runtime(&assembler.evaluation_runtime(), "\\task -> .task.join task");
    let join = |task: Value| {
        assembler
            .apply(
                &join,
                [PublicValue::from_core(&assembler.core_values(), task)],
            )
            .expect("task.join should apply to a task handle")
    };

    let join_complete = join(handle("complete"));
    let (complete_observer, complete_join) =
        schedule_composed_test_task(&assembler, &join_complete, host.clone());
    let EvaluationWaitPoll::Complete(complete_result) =
        pump_composed_test_task(&complete_observer, &complete_join)
    else {
        panic!("a same-runtime observer should join a completed task")
    };
    assert_eq!(
        complete_result.as_core(),
        &Value::binary_from_text("complete result")
    );

    let join_failed = join(handle("failed"));
    let (failed_observer, failed_join) =
        schedule_composed_test_task(&assembler, &join_failed, host.clone());
    assert!(matches!(
        pump_composed_test_task(&failed_observer, &failed_join),
        EvaluationWaitPoll::Failed(error)
            if error.to_string().contains("failed permanently")
    ));
    assert_eq!(
        owner.task_registry_counts().unacknowledged_failures,
        0,
        "propagating the child failure must acknowledge its producer-owner ledger"
    );
    assert_eq!(
        failed_observer
            .task_registry_counts()
            .unacknowledged_failures,
        1,
        "the failed joining task must enter only the observer's ledger"
    );

    let join_canceled = join(handle("canceled"));
    let (canceled_observer, canceled_join) =
        schedule_composed_test_task(&assembler, &join_canceled, host.clone());
    assert!(matches!(
        pump_composed_test_task(&canceled_observer, &canceled_join),
        EvaluationWaitPoll::Failed(error)
            if error.to_string() == "joined reflection task was cancelled"
    ));

    let join_pending = join(handle("pending"));
    let (pending_observer, pending_join) =
        schedule_composed_test_task(&assembler, &join_pending, host.clone());
    assert_eq!(
        pending_observer.pump_wait(pending_join.wait(), 16_384),
        crate::evaluation::EvaluationPumpOutcome::NoProgress,
        "a same-runtime join must remain blocked while its exact child wait is pending"
    );
    assert!(matches!(
        pending_observer.poll_reflection_task(&pending_join),
        EvaluationWaitPoll::Pending(_)
    ));

    let (_, release_pending) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        ".heap.set ['join_ready] \"ready\"",
    );
    let (release_context, release_task) =
        schedule_composed_test_task(&assembler, &release_pending, host.clone());
    assert!(matches!(
        pump_composed_test_task(&release_context, &release_task),
        EvaluationWaitPoll::Complete(_)
    ));
    let EvaluationWaitPoll::Complete(pending_result) =
        pump_composed_test_task(&pending_observer, &pending_join)
    else {
        panic!("the same-runtime join should resume when its child completes")
    };
    assert_eq!(
        pending_result.as_core(),
        &Value::binary_from_text("pending result")
    );

    let join_abandoned = join(handle("abandoned"));
    let (abandoned_observer, abandoned_join) =
        schedule_composed_test_task(&assembler, &join_abandoned, host);
    assert_eq!(
        abandoned_observer.pump_wait(abandoned_join.wait(), 16_384),
        crate::evaluation::EvaluationPumpOutcome::NoProgress
    );
    drop(owner);
    assert!(matches!(
        pump_composed_test_task(&abandoned_observer, &abandoned_join),
        EvaluationWaitPoll::Failed(error)
            if error.to_string()
                == "joined reflection task was abandoned when its evaluation session closed"
    ));
}

#[test]
fn join_does_not_advance_an_alternative_while_the_child_is_nonterminal() {
    let (assembler, effect) = compile_effect(
        ".task.new (.cut (.heap.get ['never] >>= (\\_ -> .fail))) >>= (\\task -> .cut (.alt (.task.join task) (.r \"fallback\")))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host);
    assert_eq!(
        context.pump_wait(task.wait(), 16_384),
        crate::evaluation::EvaluationPumpOutcome::NoProgress
    );
    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Pending(_)
    ));
}

#[test]
fn join_does_not_advance_an_alternative_while_the_child_waits_to_exit() {
    let (assembler, effect) = compile_effect(
        ".task.new (.exit.success) >>= (\\task -> .cut (.alt (.task.join task) (.r \"fallback\")))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_exit_child_test_task(&assembler, &effect, host);

    assert_eq!(
        context.pump_wait(task.wait(), 16_384),
        crate::evaluation::EvaluationPumpOutcome::NoProgress
    );
    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Pending(_)
    ));
    let EvaluationSessionRun::Deadlocked(report) = context.run_until_quiescent() else {
        panic!("the parent and exit-waiting child should remain unfinished")
    };
    assert!(report.failures.is_empty());
    assert_eq!(report.unfinished.len(), 2);
}

#[test]
fn cancellation_is_transactional_and_late_cancellation_is_harmless() {
    let (assembler, rolled_back) = compile_effect(
        ".task.new (.r \"alive\") >>= (\\task -> (.cut (.alt ((.task.cancel task) =>> .fail) (.r ()))) =>> .task.join task)",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &rolled_back, host.clone());
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("rolled-back cancellation should not cancel the child")
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*value))
            .unwrap(),
        b"alive".as_slice()
    );

    let (_, committed) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        ".cut (.task.new (.log 'error { msg:{ text:\"cancelled task ran\" } }) >>= (\\task -> (.task.cancel task) =>> .r task)) >>= (\\task -> .task.status task >>= (\\status -> (status == 'canceled) =>> .r ()))",
    );
    let (context, task) = schedule_composed_test_task(&assembler, &committed, host.clone());
    assert!(matches!(
        pump_composed_test_task(&context, &task),
        EvaluationWaitPoll::Complete(_)
    ));
    assert!(
        host.diagnostics().is_empty(),
        "serial commit should cancel a same-transaction task before polling it"
    );

    let (_, spawn_non_owner) =
        compile_effect_with_runtime(&assembler.evaluation_runtime(), ".task.new (.r ())");
    let (source_context, source_task) =
        schedule_composed_test_task(&assembler, &spawn_non_owner, host.clone());
    let EvaluationWaitPoll::Complete(non_owner_handle) =
        pump_composed_test_task(&source_context, &source_task)
    else {
        panic!("source session should produce a task handle for the non-owner test")
    };
    let (_, cancel_non_owner) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        "\\task -> .cut (.task.cancel task) =>> .task.status task >>= (\\status -> (status == 'canceled) =>> .r ())",
    );
    let cancel_non_owner = assembler
        .apply(
            &cancel_non_owner,
            [PublicValue::from_runtime_root(*non_owner_handle)],
        )
        .expect("non-owner cancellation should apply");
    let (non_owner_context, non_owner_task) =
        schedule_composed_test_task(&assembler, &cancel_non_owner, host.clone());
    assert!(matches!(
        pump_composed_test_task(&non_owner_context, &non_owner_task),
        EvaluationWaitPoll::Complete(_)
    ));

    let (_, late) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        ".task.new (.r \"done\") >>= (\\task -> .task.join task >>= (\\value -> (.task.cancel task) =>> .r value))",
    );
    let (context, task) = schedule_composed_test_task(&assembler, &late, host);
    assert!(matches!(
        pump_composed_test_task(&context, &task),
        EvaluationWaitPoll::Complete(_)
    ));
}

#[test]
fn same_transaction_cancellation_prevents_worker_launch_and_machine_construction() {
    let (assembler, effect) = compile_effect(
        ".cut (.task.new (.log 'error { msg:{ text:\"cancelled task ran\" } }) >>= (\\task -> (.task.cancel task) =>> .r task)) >>= (\\task -> .task.status task >>= (\\status -> (status == 'canceled) =>> .r ()))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(2).expect("test executor should start");
    let session = EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(&session);
    let builds = Arc::new(AtomicUsize::new(0));
    let launcher: Arc<dyn ReflectionTaskLauncher> = Arc::new(CountingLauncher {
        inner: task_launcher(TestEffects, host.clone()),
        builds: builds.clone(),
    });
    context
        .install_reflection_launcher(launcher)
        .expect("fresh test session should accept its launcher");
    let effect = effect.as_core().clone();
    let task = context
        .schedule_task(move |task_context| {
            EffectTask::new_in_context(effect, TestEffects, host.clone(), task_context)
                .map(|task| Box::new(ValueEffectTask(task)) as Box<dyn EvaluationTaskMachine>)
                .map_err(|error| Arc::from(error.to_string()))
        })
        .expect("parent task should schedule");

    let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
        panic!("atomically cancelled child should leave no unfinished work")
    };
    assert!(report.failures.is_empty());
    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Complete(_)
    ));
    assert_eq!(
        builds.load(Ordering::Acquire),
        0,
        "same-transaction cancellation must bypass child machine construction"
    );
}

#[test]
fn reflection_task_launch_is_buffered_until_cut_commit() {
    let (assembler, effect) =
        compile_effect(".cut (.task.new (.r \"committed\")) >>= (\\task -> .task.join task)");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host);
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("committed child task should complete")
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*value))
            .unwrap(),
        b"committed".as_slice()
    );
}

#[test]
fn failed_transaction_discards_its_reflection_task_launch_and_cancellation() {
    let (assembler, effect) = compile_effect(
        ".cut (.alt (.task.new (.log 'error { msg:{ text:\"discarded\" } }) >>= (\\task -> (.task.cancel task) =>> .fail)) (.r \"kept\"))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host.clone());
    let poll = pump_composed_test_task(&context, &task);
    assert!(
        matches!(poll, EvaluationWaitPoll::Complete(_)),
        "winning alternative should complete, got {poll:?}"
    );
    assert!(host.diagnostics().is_empty());
    assert_eq!(context.reflection_task_count(), 0);
}

#[test]
fn join_propagates_task_error_and_task_error_extracts_it() {
    let (assembler, join) = compile_effect(".task.new (.fail) >>= (\\task -> .task.join task)");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &join, host.clone());
    let EvaluationWaitPoll::Failed(error) = pump_composed_test_task(&context, &task) else {
        panic!("join should propagate its child task error")
    };
    assert!(error.to_string().contains("failed permanently"));
    let [frame] = error.contexts() else {
        panic!("join should prepend exactly one propagation frame")
    };
    let Value::Dict(frame) = frame else {
        panic!("join propagation context should be a tagged dictionary")
    };
    let Value::Dict(task_context) = frame
        .get(&Key::atom_from_text("task"))
        .expect("join propagation context should be tagged task")
    else {
        panic!("task context payload should be a dictionary")
    };
    assert_eq!(
        task_context.get(&Key::atom_from_text("operation")),
        Some(&Value::Atom(Atom::from_key(&Key::binary_from_text("join"))))
    );
    let Some(Value::Number(id)) = task_context.get(&Key::atom_from_text("id")) else {
        panic!("join propagation context should identify the child task")
    };
    assert!(id.to_u64_if_integer().is_some_and(|id| id > 0));

    let (_, extract) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        ".task.new (.fail) >>= (\\task -> .task.error task)",
    );
    let (context, task) = schedule_composed_test_task(&assembler, &extract, host);
    let poll = pump_composed_test_task(&context, &task);
    let EvaluationWaitPoll::Complete(value) = poll else {
        panic!("task_error should return the child task error, got {poll:?}")
    };
    let value = PublicValue::from_runtime_root(*value);
    let text = assembler
        .to_binary(
            &assembler
                .get(&value, "msg.text")
                .expect("task error should have a diagnostic text view"),
        )
        .expect("task error diagnostic text should be binary");
    assert!(String::from_utf8_lossy(&text).contains("failed permanently"));
}

#[test]
fn acknowledged_task_failure_remains_observable_but_is_not_reported() {
    let (assembler, inspect) = compile_effect(
        ".cut (.task.new (.fail) >>= (\\task -> .task.ack_error task =>> .r task)) >>= (\\task -> .task.error task >>= (\\error -> .task.status task >>= (\\status -> .r {error:error, status:status})))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, parent) = schedule_composed_test_task(&assembler, &inspect, host.clone());
    let EvaluationWaitPoll::Complete(observation) = pump_composed_test_task(&context, &parent)
    else {
        panic!("acknowledged failure should remain observable as data")
    };
    let observation = PublicValue::from_runtime_root(*observation);
    let error = assembler
        .evaluate(
            &assembler
                .get(&observation, "error")
                .expect("task.error should retain the failure"),
        )
        .expect("task.error result should evaluate");
    let status_error = assembler
        .evaluate(
            &assembler
                .get(&observation, "status.err")
                .expect("task.status should retain the failure"),
        )
        .expect("task.status error should evaluate");
    assert_eq!(error, status_error);
    let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
        panic!("acknowledged child should leave no unfinished work")
    };
    assert!(report.failures.is_empty());

    let (_, join) = compile_effect(
        ".cut (.task.new (.fail) >>= (\\task -> .task.ack_error task =>> .r task)) >>= (\\task -> .task.join task)",
    );
    let (context, parent) = schedule_composed_test_task(&assembler, &join, host);
    assert!(matches!(
        pump_composed_test_task(&context, &parent),
        EvaluationWaitPoll::Failed(_)
    ));
    let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
        panic!("failed join should be terminal")
    };
    assert_eq!(
        report
            .failures
            .iter()
            .map(|(task, _)| *task)
            .collect::<Vec<_>>(),
        [parent.id()],
        "join must propagate the error while the acknowledged child stays out of reports"
    );
}

#[test]
fn abandoned_task_error_acknowledgement_does_not_suppress_reporting() {
    let (assembler, effect) = compile_effect(
        ".task.new (.fail) >>= (\\task -> (.cut (.alt ((.task.ack_error task) =>> .fail) (.r ()))) =>> .task.error task)",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, parent) = schedule_composed_test_task(&assembler, &effect, host);
    assert!(matches!(
        pump_composed_test_task(&context, &parent),
        EvaluationWaitPoll::Complete(_)
    ));
    let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
        panic!("child failure should be terminal")
    };
    assert_eq!(
        report.failures.size(),
        1,
        "rolled-back acknowledgement must leave the child failure unacknowledged"
    );
    assert!(!report.failures.contains_key(&parent.id()));
}

#[test]
fn task_errors_preserve_structured_emissions_and_contexts() {
    let (assembler, effect) = compile_effect(
        ".task.new (anno context:\"child dispatch\" (anno 'error { msg:{ text:\"handler failed\" }, operation:'emit })) >>= (\\task -> .task.error task)",
    );
    let (context, task) = schedule_composed_test_task(
        &assembler,
        &effect,
        Arc::new(TestHost::with_values(assembler.core_values())),
    );
    let EvaluationWaitPoll::Complete(error) = pump_composed_test_task(&context, &task) else {
        panic!("task.error should return the child's structured failure");
    };
    let error = PublicValue::from_runtime_root(*error);
    assert_eq!(
        assembler
            .to_binary(&assembler.get(&error, "msg.text").unwrap())
            .unwrap(),
        b"handler failed".as_slice()
    );
    assert_eq!(
        assembler.get(&error, "operation").unwrap(),
        assembler.values().atom_from_text("emit")
    );
    let contexts = assembler.get(&error, "msg.context").unwrap();
    let Value::List(contexts) = contexts.as_core() else {
        panic!("task error contexts should be a list")
    };
    assert_eq!(
        eval::list_to_value_items(&assembler.eval_context(), contexts).unwrap(),
        [
            eval::evaluation_context_frame("net_computation"),
            Value::binary_from_text("child dispatch"),
            eval::evaluation_context_frame("net_computation"),
        ]
    );
}

#[test]
fn task_halt_conversions_preserve_evaluation_and_public_error_structure() {
    let assembler = Assembler::default();
    let frame = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("host"),
        Value::binary_from_text("conversion"),
    ));
    let emission = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(Dict::new_sync().insert(
                    (*keys::TEXT).clone(),
                    Value::binary_from_text("converted failure"),
                )),
            )
            .insert(
                Key::atom_from_text("detail"),
                Value::Number(Number::from(7)),
            ),
    );
    let failure = Arc::new(EvaluationFailure::emission(emission).with_context(frame.clone()));

    let evaluation_halt = TaskHalt::from(EvaluationHalt::failure(failure.clone()));
    let evaluation_diagnostic = evaluation_halt.diagnostic(&assembler.values());
    assert_eq!(evaluation_diagnostic.message(), "converted failure");
    assert_eq!(
        task_halt_contexts(&assembler, &evaluation_halt),
        std::slice::from_ref(&frame)
    );
    assert_eq!(
        assembler
            .get(evaluation_diagnostic.emission(), "detail")
            .unwrap()
            .as_i64(),
        Some(7)
    );

    let public_error =
        ApiError::from_eval(&assembler.core_values(), EvaluationHalt::failure(failure));
    let public_halt = TaskHalt::from(public_error);
    let public_diagnostic = public_halt.diagnostic(&assembler.values());
    assert_eq!(public_diagnostic.message(), "converted failure");
    assert_eq!(task_halt_contexts(&assembler, &public_halt), [frame]);
    assert_eq!(
        assembler
            .get(public_diagnostic.emission(), "detail")
            .unwrap()
            .as_i64(),
        Some(7)
    );
}

#[test]
fn effect_dispatch_preserves_structured_failure_and_adds_stage_context() {
    let (assembler, effect) = compile_effect(
        "{eff:anno context:\"effect function\" (anno 'error {msg:{text:\"dispatch failed\"}, detail:7})}",
    );

    let halt = run_standard_test(&assembler, &effect).expect_err("the effect function should fail");
    let diagnostic = halt.diagnostic(&assembler.values());
    assert_eq!(diagnostic.message(), "dispatch failed");
    assert_eq!(
        assembler
            .get(diagnostic.emission(), "detail")
            .expect("dispatch should preserve ad hoc diagnostic fields")
            .as_i64(),
        Some(7)
    );

    let contexts = task_halt_contexts(&assembler, &halt);
    assert_eq!(
        contexts.first(),
        Some(&effect_dispatch_context("function")),
        "the dispatch boundary should prepend its stage"
    );
    assert!(
        contexts.contains(&Value::binary_from_text("effect function")),
        "the original effect-function context should survive"
    );
}

#[test]
fn failed_and_cancelled_joins_retain_retryable_errors() {
    for (source, expected) in [
        (
            ".task.new (.fail) >>= (\\task -> .cut (.heap.get ['observed] >>= (\\_ -> .task.join task)))",
            "failed permanently",
        ),
        (
            ".task.new (.r ()) >>= (\\task -> (.task.cancel task) =>> .cut (.heap.get ['observed] >>= (\\_ -> .task.join task)))",
            "was cancelled",
        ),
    ] {
        let (assembler, effect) = compile_effect(source);
        let host = Arc::new(TestHost::with_values(assembler.core_values()));
        let (context, parent) = schedule_composed_test_task(&assembler, &effect, host.clone());
        let EvaluationSessionRun::Deadlocked(report) = context.run_until_quiescent() else {
            panic!("retryable joined task error should remain unfinished for {source}")
        };
        let blocked = report
            .unfinished
            .iter()
            .find(|task| task.task == parent.id())
            .expect("parent task should retain its joined error");
        assert!(blocked.wait.is_none());
        assert!(blocked.observed_epoch.is_some());
        assert!(
            blocked
                .error
                .as_ref()
                .is_some_and(|error| error.to_string().contains(expected)),
            "unexpected retained error for {source}: {:?}",
            blocked.error
        );
    }
}

#[test]
fn observed_evaluation_error_restarts_without_advancing_alternatives() {
    let (assembler, effect) = compile_effect(".cut (.alt (.read_log) (1 2))");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut task = EffectTask::new(
        &assembler.core_values(),
        effect.as_core().clone(),
        TestEffects,
        host.clone(),
    )
    .unwrap();

    let EffectTaskPoll::Blocked(blocked) = task.poll(512) else {
        panic!("error after an observed alternative should remain retryable")
    };
    assert!(blocked.lazy.is_none());
    assert!(blocked.observed_generation.is_some());
    assert!(
        blocked
            .error
            .as_ref()
            .is_some_and(|error| error.to_string().contains("requires a function value")),
        "unexpected retained error: {:?}",
        blocked.error
    );

    host.emit_diagnostic(Diagnostic::new(
        &assembler.values(),
        crate::diagnostic::Severity::Info,
        "available after retry",
    ));
    let value = loop {
        match task.poll(512) {
            EffectTaskPoll::Yielded => {}
            EffectTaskPoll::Complete(value) => break value,
            EffectTaskPoll::Blocked(_) => panic!("changed observation did not retry the cut"),
            EffectTaskPoll::Failed(error) => panic!("retryable task failed: {error}"),
            EffectTaskPoll::Cancelled => panic!("retryable task was cancelled"),
            EffectTaskPoll::Exit(_) => panic!("retryable task unexpectedly voted to exit"),
        }
    };
    assert_eq!(
        assembler.get(&value, "msg.text").unwrap(),
        assembler.values().text("available after retry")
    );
}

#[test]
fn synchronous_error_recovery_waits_for_observed_state_change() {
    let (assembler, effect) =
        compile_effect(".cut (.heap.get ['handler] >>= (\\handler -> handler ()))");
    let (_, handler) =
        compile_effect_with_runtime(&assembler.evaluation_runtime(), "\\_ -> .r \"recovered\"");
    let host = Arc::new(TestHost::with_wake_heap(
        assembler.core_values(),
        public_record(&assembler, [("handler", handler)]),
    ));
    let mut task = EffectTask::new(
        &assembler.core_values(),
        effect.as_core().clone(),
        TestEffects,
        host.clone(),
    )
    .unwrap();

    let TaskOutcome::Complete(value) = task.run().unwrap() else {
        panic!("state change should recover the evaluation error")
    };
    assert_eq!(
        assembler.to_binary(&value).unwrap(),
        b"recovered".as_slice()
    );
    assert_eq!(host.wait_count(), 1);
}

#[test]
fn unobserved_evaluation_error_remains_terminal_inside_cut() {
    let (assembler, effect) = compile_effect(".cut (.alt (1 2) (.r \"fallback\"))");
    let error =
        run_standard_test(&assembler, &effect).expect_err("unobserved error should be terminal");
    assert!(error.to_string().contains("requires a function value"));
}

#[test]
fn task_error_fails_for_a_successful_task() {
    let (assembler, effect) = compile_effect(".task.new (.r ()) >>= (\\task -> .task.error task)");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host);
    let EvaluationWaitPoll::Failed(error) = pump_composed_test_task(&context, &task) else {
        panic!("task_error should fail for a successful task")
    };
    assert!(error.to_string().contains("failed permanently"));
}

#[test]
fn pending_task_error_is_an_effect_failure_before_it_is_a_wait() {
    let (assembler, effect) = compile_effect(
        ".task.new (.r \"child\") >>= (\\task -> .cut (.alt (.task.error task) (.r \"fallback\")))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host);
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("task_error alternative should fall through")
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*value))
            .unwrap(),
        b"fallback".as_slice()
    );
}

#[test]
fn spawned_tasks_inherit_the_parent_task_profile() {
    let (assembler, effect) = compile_effect(
        ".task.new ((.write_stderr \"child\") =>> .r \"done\") >>= (\\task -> .task.join task)",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host.clone());
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("child should inherit the parent's extended effect vocabulary")
    };
    assert_eq!(
        assembler
            .to_binary(&PublicValue::from_runtime_root(*value))
            .unwrap(),
        b"done".as_slice()
    );
    assert_eq!(host.stderr(), [Bytes::from_static(b"child")]);
}

#[test]
fn polling_reports_state_block_without_waiting_in_the_machine() {
    let (assembler, effect) = compile_effect(".read_log");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut task = EffectTask::new(
        &assembler.core_values(),
        effect.as_core().clone(),
        TestEffects,
        host.clone(),
    )
    .unwrap();

    let EffectTaskPoll::Blocked(blocked) = task.poll(256) else {
        panic!("empty queue should suspend the task")
    };
    assert!(blocked.lazy.is_none());
    assert!(blocked.observed_generation.is_some());
    assert_eq!(host.wait_count(), 0);

    host.emit_diagnostic(Diagnostic::new(
        &assembler.values(),
        crate::diagnostic::Severity::Info,
        "available now",
    ));
    assert!(matches!(task.poll(256), EffectTaskPoll::Complete(_)));
    assert_eq!(host.wait_count(), 0);
}

#[test]
fn lazy_suspension_preserves_cut_choice_and_does_not_repeat_prior_commit() {
    let (assembler, build_effect) = compile_effect(
        "\\x -> (.write_stderr \"once\") =>> .cut (.alt (.r x >>= (\\value -> (value == \"done\") =>> .r value)) ((.write_stderr \"wrong\") =>> .r \"wrong\"))",
    );
    let gate = PublicValue::from_core(
        &assembler.core_values(),
        Value::Lazy(LazyValue::from_reflection_gate(
            &crate::core::test_value_factory(),
            Value::Number(Number::from_u64(0)),
            Value::binary_from_text("done"),
        )),
    );
    let effect = assembler.apply(&build_effect, [gate]).unwrap();
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut task = EffectTask::new(
        &assembler.core_values(),
        effect.as_core().clone(),
        TestEffects,
        host.clone(),
    )
    .unwrap();

    let blocked = match task.poll(512) {
        EffectTaskPoll::Blocked(blocked) => blocked,
        EffectTaskPoll::Yielded => panic!("task exhausted an unexpectedly large poll budget"),
        EffectTaskPoll::Complete(value) => panic!(
            "annotation dependency completed early with {:?}",
            assembler.to_binary(&value)
        ),
        EffectTaskPoll::Failed(error) => panic!("annotation dependency failed: {error}"),
        EffectTaskPoll::Cancelled => panic!("annotation dependency was cancelled"),
        EffectTaskPoll::Exit(_) => panic!("annotation dependency unexpectedly voted to exit"),
    };
    let wait = blocked
        .lazy
        .expect("lazy suspension should retain its wait token");
    assert_eq!(host.stderr(), [Bytes::from_static(b"once")]);

    task.eval_context.complete_wait(&wait);
    let value = loop {
        match task.poll(512) {
            EffectTaskPoll::Yielded => {}
            EffectTaskPoll::Complete(value) => break value,
            EffectTaskPoll::Blocked(_) => panic!("completed dependency remained blocked"),
            EffectTaskPoll::Failed(error) => panic!("resumed task failed: {error}"),
            EffectTaskPoll::Cancelled => panic!("resumed task was cancelled"),
            EffectTaskPoll::Exit(_) => panic!("resumed task unexpectedly voted to exit"),
        }
    };
    assert_eq!(assembler.to_binary(&value).unwrap(), b"done".as_slice());
    assert_eq!(host.stderr(), [Bytes::from_static(b"once")]);
}

#[test]
fn changed_observation_restarts_a_cut_before_its_lazy_dependency() {
    let (assembler, build_effect) = compile_effect(
        "\\x -> .cut (.alt (.read_log >>= (\\message -> .r message.msg.text)) (.r x >>= (\\value -> (value == \"unused\") =>> .r value)))",
    );
    let gate = PublicValue::from_core(
        &assembler.core_values(),
        Value::Lazy(LazyValue::from_reflection_gate(
            &crate::core::test_value_factory(),
            Value::Number(Number::from_u64(0)),
            Value::binary_from_text("unused"),
        )),
    );
    let effect = assembler.apply(&build_effect, [gate]).unwrap();
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let mut task = EffectTask::new(
        &assembler.core_values(),
        effect.as_core().clone(),
        TestEffects,
        host.clone(),
    )
    .unwrap();

    let EffectTaskPoll::Blocked(blocked) = task.poll(512) else {
        panic!("right alternative should retain the failed queue observation")
    };
    assert!(blocked.lazy.is_some());
    assert!(blocked.observed_generation.is_some());

    host.emit_diagnostic(Diagnostic::new(
        &assembler.values(),
        crate::diagnostic::Severity::Info,
        "state won",
    ));
    let value = loop {
        match task.poll(512) {
            EffectTaskPoll::Yielded => {}
            EffectTaskPoll::Complete(value) => break value,
            EffectTaskPoll::Blocked(_) => panic!("changed observation did not restart cut"),
            EffectTaskPoll::Failed(error) => panic!("restarted cut failed: {error}"),
            EffectTaskPoll::Cancelled => panic!("restarted cut was cancelled"),
            EffectTaskPoll::Exit(_) => panic!("restarted cut unexpectedly voted to exit"),
        }
    };
    assert_eq!(
        assembler.to_binary(&value).unwrap(),
        b"state won".as_slice()
    );
}

#[test]
fn runs_return_sequence_and_fixpoint_requests() {
    let (assembler, value) = completed(".fix (\\_loop -> .r \"A\") >>= (\\x -> .r (x ++ \"B\"))");
    assert_eq!(assembler.to_binary(&value).unwrap(), b"AB".as_slice());
}

#[test]
fn unobserved_failure_is_permanent_with_or_without_cut() {
    for source in [".fail", ".cut (.fail)"] {
        let (assembler, effect) = compile_effect(source);
        let host = Arc::new(TestHost::with_values(assembler.core_values()));
        assert!(
            run_log_test(&assembler, &effect, host.clone())
                .unwrap_err()
                .to_string()
                .contains("failed permanently")
        );
        assert_eq!(host.wait_count(), 0, "`{source}` must not wait");
    }
}

#[test]
fn empty_log_read_outside_cut_retries_after_its_observation_changes() {
    let (assembler, effect) = compile_effect(".read_log >>= (\\message -> .r message.msg.text)");
    let host = Arc::new(TestHost::with_wake_diagnostic(
        assembler.core_values(),
        Diagnostic::new(
            &assembler.values(),
            crate::diagnostic::Severity::Warning,
            "arrived later",
        ),
    ));
    let TaskOutcome::Complete(value) = run_log_test(&assembler, &effect, host.clone()).unwrap()
    else {
        panic!("observed queue change should resume the log read")
    };
    assert_eq!(
        assembler.to_binary(&value).unwrap(),
        b"arrived later".as_slice()
    );
    assert_eq!(host.wait_count(), 1);
    assert!(host.diagnostics().is_empty());
}

#[test]
fn committed_log_read_clears_its_retry_checkpoint() {
    let (assembler, effect) = compile_effect(".read_log >>= (\\_message -> .fail)");
    let host = Arc::new(TestHost::with_diagnostics(
        assembler.core_values(),
        vec![Diagnostic::new(
            &assembler.values(),
            crate::diagnostic::Severity::Warning,
            "consumed once",
        )],
    ));
    assert!(
        run_log_test(&assembler, &effect, host.clone())
            .unwrap_err()
            .to_string()
            .contains("failed permanently")
    );
    assert_eq!(host.wait_count(), 0);
    assert!(host.diagnostics().is_empty());
}

#[test]
fn cut_retries_only_after_a_failed_alternative_observes_changeable_state() {
    let (assembler, effect) =
        compile_effect(".cut (.alt (.read_log >>= (\\message -> .r message.msg.text)) (.fail))");
    let host = Arc::new(TestHost::with_wake_diagnostic(
        assembler.core_values(),
        Diagnostic::new(
            &assembler.values(),
            crate::diagnostic::Severity::Warning,
            "cut resumed",
        ),
    ));
    let TaskOutcome::Complete(value) = run_log_test(&assembler, &effect, host.clone()).unwrap()
    else {
        panic!("observed queue change should restart the exhausted cut")
    };
    assert_eq!(
        assembler.to_binary(&value).unwrap(),
        b"cut resumed".as_slice()
    );
    assert_eq!(host.wait_count(), 1);
}

#[test]
fn standard_task_does_not_expose_specialized_requests() {
    let (assembler, effect) = compile_effect(".read_log");
    assert!(run_standard_test(&assembler, &effect).is_err());

    let (assembler, effect) = compile_effect(".log 'info { msg:{ text:\"hidden\" } }");
    assert!(run_standard_test(&assembler, &effect).is_err());
}

#[test]
fn reusable_reflection_api_does_not_expose_internal_queries() {
    let (assembler, effect) = compile_effect(".query.result {}");
    assert!(
        run_reflection_test(
            &assembler,
            &effect,
            Arc::new(TestHost::with_values(assembler.core_values()))
        )
        .is_err()
    );
}

#[test]
fn reusable_reflection_log_emits_raw_diagnostics_transactionally() {
    let (assembler, effect) =
        compile_effect(".cut ((.log 'warn { msg:{ text:\"reflection warning\" } }) =>> .r ())");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    assert!(matches!(
        run_reflection_test(&assembler, &effect, host.clone()).unwrap(),
        TaskOutcome::Complete(_)
    ));
    let diagnostics = host.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].severity(),
        crate::diagnostic::Severity::Warning
    );
    let enriched = diagnostics[0].enrich(&assembler.values()).unwrap();
    let text = assembler.get(&enriched, "msg.text").unwrap();
    assert_eq!(
        assembler.to_binary(&text).unwrap(),
        b"reflection warning".as_slice()
    );

    let (_, invalid) = compile_effect(".log 'verbose { msg:{ text:\"wrong\" } }");
    assert!(
        run_reflection_test(&assembler, &invalid, host)
            .unwrap_err()
            .to_string()
            .contains("severity must be")
    );
}

#[test]
fn reflection_log_contextualizes_nested_message_and_severity_failures() {
    let (message_assembler, message_effect) =
        compile_effect(".log 'info (anno 'error \"message construction failed\")");
    let message_error = run_reflection_test(
        &message_assembler,
        &message_effect,
        Arc::new(TestHost::with_values(message_assembler.core_values())),
    )
    .unwrap_err();
    assert_eq!(
        task_halt_contexts(&message_assembler, &message_error),
        [
            eval::evaluation_context_frame("log_message"),
            eval::evaluation_context_frame("net_computation"),
        ]
    );

    let (severity_assembler, severity_effect) = compile_effect(
        ".log (anno 'error \"severity construction failed\") { msg:{ text:\"unused\" } }",
    );
    let severity_error = run_reflection_test(
        &severity_assembler,
        &severity_effect,
        Arc::new(TestHost::with_values(severity_assembler.core_values())),
    )
    .unwrap_err();
    assert_eq!(
        task_halt_contexts(&severity_assembler, &severity_error),
        [
            eval::evaluation_context_frame("log_severity"),
            eval::evaluation_context_frame("net_computation"),
        ]
    );
}

#[test]
fn fixpoint_alternatives_receive_independent_futures() {
    let (assembler, value) = completed(
        ".cut (.fix (\\_loop -> .alt (.alt (.r \"left\") (.r \"middle\")) (.r \"right\")) >>= (\\value -> (value == \"right\") =>> .r value))",
    );
    assert_eq!(assembler.to_binary(&value).unwrap(), b"right".as_slice());

    let (assembler, value) = completed(".fix (\\_loop -> .cut (.alt (.fail) (.r \"nested cut\")))");
    assert_eq!(
        assembler.to_binary(&value).unwrap(),
        b"nested cut".as_slice()
    );

    let (assembler, value) = completed(
        ".cut (.fix (\\outer -> .fix (\\inner -> .alt (.r \"nested left\") (.r \"nested right\"))) >>= (\\value -> (value == \"nested right\") =>> .r value))",
    );
    assert_eq!(
        assembler.to_binary(&value).unwrap(),
        b"nested right".as_slice()
    );
}

#[test]
fn reflection_fixpoint_reports_recursive_self_observation() {
    let (assembler, effect) = compile_effect(".fix (\\recur -> recur)");
    let error = run_standard_test(&assembler, &effect).unwrap_err();
    assert!(
        error.to_string().contains("recursively observed itself"),
        "{error}"
    );
}

#[test]
fn task_failure_propagates_one_structured_failure_to_owned_promises() {
    let (assembler, _effect) = compile_effect(".r ()");
    let context = EvalContext::isolated(assembler.core_values());
    let (promises, owner_task, owner) = context
        .task_owned_promises([
            Arc::from("first owned promise"),
            Arc::from("second owned promise"),
            Arc::from("resolved owned promise"),
        ])
        .unwrap();
    let mut promises = promises.into_iter();
    let unresolved = [
        promises.next().expect("first promise should exist"),
        promises.next().expect("second promise should exist"),
    ];
    let resolved = promises.next().expect("resolved promise should exist");
    let waits = unresolved.each_ref().map(|promise| {
        promise
            .task()
            .expect("task-owned promise should expose its wait")
            .wait()
            .clone()
    });
    let resolved_wait = resolved
        .task()
        .expect("task-owned promise should expose its wait")
        .wait()
        .clone();
    resolved
        .set(Value::Number(Number::integer(42)))
        .expect("one owned promise should resolve before task failure");

    let detail = Key::atom_from_text("detail");
    let emission = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(Dict::new_sync().insert(
                    (*keys::TEXT).clone(),
                    Value::binary_from_text("structured producer failure"),
                )),
            )
            .insert(detail, Value::Number(Number::integer(7))),
    );
    let frame = eval::evaluation_context_frame("producer_test");
    let failure =
        Arc::new(EvaluationFailure::emission(emission.clone()).with_context(frame.clone()));
    context.fail_wait_with_failure(owner_task.wait(), failure.clone());

    for (promise, wait) in unresolved.into_iter().zip(waits) {
        let observed = eval::eval_value(&owner, &Value::Promised(promise))
            .expect_err("unresolved owned promise should inherit producer failure")
            .into_permanent_failure();
        assert!(Arc::ptr_eq(&failure, &observed));
        assert_eq!(observed.emission_value(), Some(&emission));
        assert_eq!(observed.contexts(), std::slice::from_ref(&frame));
        let EvaluationWaitPoll::Failed(wait_failure) = owner.poll_wait(&wait) else {
            panic!("owned promise wait should publish the producer failure")
        };
        assert!(Arc::ptr_eq(&failure, &wait_failure));
    }

    assert_eq!(
        eval::eval_value(&owner, &Value::Promised(resolved)).unwrap(),
        Value::Number(Number::integer(42)),
        "producer failure must not replace an earlier assignment"
    );
    assert_eq!(
        owner.poll_wait(&resolved_wait),
        EvaluationWaitPoll::Complete(Box::new(crate::runtime::RuntimeValueRoot::new(
            owner.values(),
            Value::Number(Number::integer(42)),
        )))
    );
    let counts = context.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn task_completion_and_cancellation_fail_unresolved_owned_promises() {
    let (assembler, _effect) = compile_effect(".r ()");
    let context = EvalContext::isolated(assembler.core_values());
    let cases = [
        (
            false,
            "reflection task completed without fulfilling its fixpoint",
        ),
        (true, "reflection fixpoint producer was cancelled"),
    ];

    for (cancel, expected) in cases {
        let (promise, owner_task, owner) = context
            .task_owned_promise(Arc::from("unfinished owned promise"))
            .unwrap();
        let wait = promise
            .task()
            .expect("task-owned promise should expose its wait")
            .wait()
            .clone();

        if cancel {
            assert_eq!(owner_task.cancel(), EvaluationTaskCancellation::Requested);
        } else {
            context.complete_wait(owner_task.wait());
        }

        let observed = eval::eval_value(&owner, &Value::Promised(promise))
            .expect_err("terminal task should fail its unfinished promise")
            .into_permanent_failure();
        assert_eq!(observed.to_string(), expected);
        let EvaluationWaitPoll::Failed(wait_failure) = owner.poll_wait(&wait) else {
            panic!("unfinished promise wait should publish its synthesized failure")
        };
        assert!(Arc::ptr_eq(&observed, &wait_failure));
        let counts = context.task_registry_counts();
        assert_eq!(counts.promises_active, 0);
        assert_eq!(counts.promises_terminal, 0);
        assert_eq!(counts.owned_promise_waits, 0);
    }
}

#[test]
fn fixpoint_hides_then_restores_the_reset_stack() {
    let (assembler, hidden) = compile_effect(
        ".reset \"prompt\" (.fix (\\_loop -> .shift \"prompt\" (\\continuation -> continuation \"wrong\")))",
    );
    assert!(
        run_standard_test(&assembler, &hidden)
            .unwrap_err()
            .to_string()
            .contains("not in reset scope")
    );

    let (assembler, value) = completed(
        ".reset \"prompt\" ((.fix (\\_loop -> .r ())) =>> .shift \"prompt\" (\\continuation -> continuation \"restored\"))",
    );
    assert_eq!(assembler.to_binary(&value).unwrap(), b"restored".as_slice());
}

#[test]
fn cut_rolls_back_failed_alternative_user_state() {
    let (assembler, value) = completed(
        ".cut (.alt ((.set [\"x\"] \"bad\") =>> .fail) ((.get [\"x\"]) >>= (\\x -> (x == {}) =>> .r \"clean\")))",
    );
    assert_eq!(assembler.to_binary(&value).unwrap(), b"clean".as_slice());
}

#[test]
fn shift_captures_only_a_matching_task_local_reset() {
    let (assembler, value) = completed(
        ".reset \"prompt\" (.shift \"prompt\" (\\continuation -> continuation \"resumed\"))",
    );
    assert_eq!(assembler.to_binary(&value).unwrap(), b"resumed".as_slice());

    let (assembler, value) = completed(
        ".reset \"prompt\" ((.cut (.r ())) =>> .shift \"prompt\" (\\continuation -> continuation \"after cut\"))",
    );
    assert_eq!(
        assembler.to_binary(&value).unwrap(),
        b"after cut".as_slice()
    );
}

#[test]
fn continuation_task_identity_prevents_cross_task_aliasing() {
    let (assembler, effect) =
        compile_effect(".reset \"prompt\" (.shift \"prompt\" (\\continuation -> .r continuation))");
    let TaskOutcome::Complete(continuation) = run_standard_test(&assembler, &effect).unwrap()
    else {
        panic!("continuation capture should complete")
    };
    let cross_task_invocation = assembler
        .apply(&continuation, [assembler.values().text("cross-task")])
        .expect("continuation should remain an applicable value");

    assert!(
        run_standard_test(&assembler, &cross_task_invocation)
            .unwrap_err()
            .to_string()
            .contains("belongs to another reflection task")
    );
}

#[test]
fn replacing_root_state_replaces_the_active_reset_stack() {
    let (assembler, effect) = compile_effect(
        ".reset \"prompt\" ((.set [] {}) =>> .shift \"prompt\" (\\continuation -> continuation \"wrong\"))",
    );
    assert!(
        run_standard_test(&assembler, &effect)
            .unwrap_err()
            .to_string()
            .contains("not in reset scope")
    );
}

#[test]
fn restoring_root_state_restores_its_reset_stack() {
    let (assembler, value) = completed(
        ".reset \"prompt\" (.get [] >>= (\\saved -> (.set [] {}) =>> (.set [] saved) =>> .shift \"prompt\" (\\continuation -> continuation \"resumed\")))",
    );
    assert_eq!(assembler.to_binary(&value).unwrap(), b"resumed".as_slice());
}

#[test]
fn reading_all_local_state_does_not_observe_shared_heap() {
    let (assembler, effect) = compile_effect(".get [] >>= (\\_state -> .fail)");
    let host = Arc::new(TestHost::with_wake_heap(
        assembler.core_values(),
        public_record(&assembler, [("changed", assembler.values().text("later"))]),
    ));
    let error = run_standard_on(&assembler, &effect, host.clone()).unwrap_err();
    assert!(error.to_string().contains("failed permanently"));
    assert_eq!(host.wait_count(), 0);
}

#[test]
fn cut_rolls_back_log_reads_and_stderr_writes_before_trying_an_alternative() {
    let (assembler, effect) = compile_effect(
        ".cut (.alt (.read_log >>= (\\message -> (.write_stderr \"bad\") =>> .fail)) (.read_log >>= (\\message -> (.write_stderr message.msg.text) =>> .r ())))",
    );
    let host = Arc::new(TestHost::with_diagnostics(
        assembler.core_values(),
        vec![Diagnostic::new(
            &assembler.values(),
            crate::diagnostic::Severity::Warning,
            "good",
        )],
    ));
    assert!(matches!(
        run_log_test(&assembler, &effect, host.clone()).unwrap(),
        TaskOutcome::Complete(_)
    ));
    assert_eq!(host.stderr(), [Bytes::from_static(b"good")]);
    assert!(
        <TestHost as TaskHost<TestEffects>>::snapshot(host.as_ref())
            .extra()
            .diagnostics
            .is_empty()
    );
}

#[test]
fn composed_logging_does_not_read_its_own_reflection_writes() {
    let (assembler, effect) = compile_effect(
        ".cut (.alt ((.log 'error { msg:{ text:\"bad\" } }) =>> (.read_log >>= (\\message -> (.write_stderr message.msg.text) =>> .r ()))) ((.log 'warn { msg:{ text:\"good\" } }) =>> .r ()))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    assert!(matches!(
        run_log_test(&assembler, &effect, host.clone()).unwrap(),
        TaskOutcome::Complete(_)
    ));
    assert!(host.stderr().is_empty());
    let diagnostics = host.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].severity(),
        crate::diagnostic::Severity::Warning
    );
    let text = assembler
        .get(
            &diagnostics[0].enrich(&assembler.values()).unwrap(),
            "msg.text",
        )
        .unwrap();
    assert_eq!(assembler.to_binary(&text).unwrap(), b"good".as_slice());
}

#[test]
fn root_local_state_replacement_does_not_replace_shared_heap() {
    let (assembler, effect) =
        compile_effect("(.set [] { heap:{ answer:\"local\" }, local:\"owned\" }) =>> .heap.get []");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let TaskOutcome::Complete(value) = run_standard_on(&assembler, &effect, host.clone()).unwrap()
    else {
        panic!("state effect should complete")
    };
    assert_eq!(value, assembler.values().empty_dict());
    assert_eq!(host.heap(), assembler.values().empty_dict());
    assert!(
        assembler
            .get(&value, "answer")
            .is_ok_and(|answer| answer.is_undefined())
    );
}

#[test]
fn child_tasks_start_with_fresh_local_state_and_share_heap() {
    let (assembler, effect) = compile_effect(
        ".heap.set ['shared] \"visible\" =>> .set ['local] \"private\" =>> .task.new (.get ['local] >>= (\\local -> .heap.get ['shared] >>= (\\shared -> .r { local:local, shared:shared }))) >>= (\\task -> .task.join task)",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let (context, task) = schedule_composed_test_task(&assembler, &effect, host);
    let EvaluationWaitPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
        panic!("child task should complete")
    };
    let value = PublicValue::from_runtime_root(*value);
    assert_eq!(
        assembler
            .evaluate(&assembler.get(&value, "local").unwrap())
            .unwrap(),
        assembler.values().empty_dict()
    );
    assert_eq!(
        assembler
            .to_binary(&assembler.get(&value, "shared").unwrap())
            .unwrap(),
        b"visible".as_slice()
    );
}

#[test]
fn child_tasks_inherit_same_session_volume_capabilities() {
    let assembler = Assembler::default();
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let volume = host
        .state
        .lock()
        .unwrap()
        .store
        .create_volume(assembler.values().text("initial"))
        .unwrap();
    let module = assembler
            .module(["reflection_volume_child"])
            .script(
                "g",
                "language g0\nimport 'std\nlaunch = \\cap -> .task.new (cap.set [] \"child\") >>= (\\task -> .task.join task)\n",
            )
            .build()
            .expect("volume child fixture should compile");
    let launch = assembler
        .get(module.value(), "launch")
        .expect("volume child fixture should define launch");
    let effect = assembler
        .apply(&launch, [volume_effects(&assembler.core_values(), volume)])
        .expect("volume capability should apply to the child launcher");
    let reflection_host: Arc<dyn ReflectionHost<ReflectionEffects>> = host.clone();

    assert!(matches!(
        EffectRun::new(
            &assembler.evaluation_runtime(),
            &effect,
            ReflectionEffects,
            reflection_host.clone(),
        )
        .run()
        .unwrap(),
        TaskOutcome::Complete(_)
    ));
    let final_value = host
        .state
        .lock()
        .unwrap()
        .store
        .snapshot()
        .volume(volume)
        .cloned()
        .expect("child write must not remove the volume");
    assert_eq!(
        assembler.to_binary(&final_value).unwrap(),
        b"child".as_slice()
    );
}

#[test]
fn failed_alternative_rolls_back_local_and_heap_changes() {
    let (assembler, effect) = compile_effect(
        ".cut (.alt ((.set ['local] \"bad\") =>> .heap.set ['shared] \"bad\" =>> .fail) (.get ['local] >>= (\\local -> .heap.get ['shared] >>= (\\shared -> .r { local:local, shared:shared }))))",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let TaskOutcome::Complete(value) = run_standard_on(&assembler, &effect, host.clone()).unwrap()
    else {
        panic!("clean alternative should complete")
    };
    assert_eq!(
        assembler
            .evaluate(&assembler.get(&value, "local").unwrap())
            .unwrap(),
        assembler.values().empty_dict()
    );
    assert_eq!(
        assembler
            .evaluate(&assembler.get(&value, "shared").unwrap())
            .unwrap(),
        assembler.values().empty_dict()
    );
    assert_eq!(host.heap(), assembler.values().empty_dict());
}

#[test]
fn blind_heap_write_does_not_make_failure_retryable() {
    let (assembler, effect) = compile_effect(".cut ((.heap.set ['discarded] \"value\") =>> .fail)");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let error = run_standard_on(&assembler, &effect, host.clone()).unwrap_err();
    assert!(error.to_string().contains("failed permanently"));
    assert_eq!(host.wait_count(), 0);
    assert_eq!(host.heap(), assembler.values().empty_dict());
}

#[test]
fn reading_a_covering_own_write_does_not_make_failure_retryable() {
    let (assembler, effect) = compile_effect(
        ".cut ((.heap.set ['owned] \"value\") =>> .heap.get ['owned] >>= (\\_value -> .fail))",
    );
    let host = Arc::new(TestHost::with_wake_heap(
        assembler.core_values(),
        public_record(&assembler, [("changed", assembler.values().text("later"))]),
    ));
    let error = run_standard_on(&assembler, &effect, host.clone()).unwrap_err();

    assert!(error.to_string().contains("failed permanently"));
    assert_eq!(host.wait_count(), 0);
    assert_eq!(host.heap(), assembler.values().empty_dict());
}

#[test]
fn heap_rewrite_lazily_updates_the_transactional_view() {
    let (assembler, effect) = compile_effect(
        ".heap.set ['items] [\"base\"] =>> .heap.rewrite ['items] (\\items -> items ++ [\"next\"]) =>> .heap.get ['items]",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let TaskOutcome::Complete(value) = run_standard_on(&assembler, &effect, host.clone()).unwrap()
    else {
        panic!("heap rewrite should complete")
    };
    let expected = public_list(
        &assembler,
        [
            assembler.values().text("base"),
            assembler.values().text("next"),
        ],
    );

    assert_list_values(&assembler, &value, &expected);
    let heap_items = assembler.get(&host.heap(), "items").unwrap();
    assert_list_values(&assembler, &heap_items, &expected);
}

#[test]
fn blind_heap_rewrite_does_not_make_failure_retryable() {
    let (assembler, effect) =
        compile_effect(".cut (.heap.rewrite ['counter] (\\value -> value + 1) =>> .fail)");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let error = run_standard_on(&assembler, &effect, host.clone()).unwrap_err();

    assert!(error.to_string().contains("failed permanently"));
    assert_eq!(host.wait_count(), 0);
    assert_eq!(host.heap(), assembler.values().empty_dict());
}

#[test]
fn heap_root_get_and_set_are_explicit_whole_heap_operations() {
    let (assembler, effect) = compile_effect(".heap.set [] { answer:\"shared\" } =>> .heap.get []");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let TaskOutcome::Complete(value) = run_standard_on(&assembler, &effect, host.clone()).unwrap()
    else {
        panic!("whole-heap operations should complete")
    };
    assert_eq!(
        assembler
            .to_binary(&assembler.get(&value, "answer").unwrap())
            .unwrap(),
        b"shared".as_slice()
    );
    assert_eq!(value, host.heap());
}

#[test]
fn heap_root_replacement_and_path_errors_remain_lazy() {
    let (assembler, effect) = compile_effect(".cut ((.heap.set [] 42) =>> .heap.get ['x])");
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    let TaskOutcome::Complete(value) = run_standard_on(&assembler, &effect, host.clone()).unwrap()
    else {
        panic!("heap access should return its latent error value")
    };
    assert!(matches!(value.as_core(), Value::Lazy(_)));
    assert_eq!(host.heap(), assembler.values().integer(42));
    assert!(
        assembler
            .evaluate(&value)
            .unwrap_err()
            .to_string()
            .contains("not a dictionary")
    );

    let (_, caught) = compile_effect(
        ".cut ((.heap.set [] 42) =>> .heap.get ['x] >>= (\\value -> .eval value >>= (\\result -> .r result.err)))",
    );
    let TaskOutcome::Complete(error) = run_reflection_test(
        &assembler,
        &caught,
        Arc::new(TestHost::with_values(assembler.core_values())),
    )
    .unwrap() else {
        panic!("reflection eval should catch a latent heap access error")
    };
    let error_text = assembler.get(&error, "msg.text").unwrap();
    assert!(
        String::from_utf8_lossy(&assembler.to_binary(&error_text).unwrap())
            .contains("not a dictionary")
    );
}

#[test]
fn malformed_nested_heap_updates_do_not_poison_unrelated_reads() {
    let (assembler, update) = compile_effect(
        ".cut ((.heap.set [] { safe:\"ok\", x:3 }) =>> (.heap.set ['x, 'b] 7) =>> .r ())",
    );
    let host = Arc::new(TestHost::with_values(assembler.core_values()));
    assert!(matches!(
        run_standard_on(&assembler, &update, host.clone()).unwrap(),
        TaskOutcome::Complete(_)
    ));

    let (_, safe) = compile_effect(".heap.get ['safe]");
    let TaskOutcome::Complete(safe) = run_standard_on(&assembler, &safe, host.clone()).unwrap()
    else {
        panic!("unrelated heap access should complete")
    };
    assert_eq!(assembler.to_binary(&safe).unwrap(), b"ok".as_slice());

    let (_, bad) = compile_effect_with_runtime(
        &assembler.evaluation_runtime(),
        ".heap.get ['x, 'b] >>= (\\value -> .eval value >>= (\\result -> .r result.err))",
    );
    let TaskOutcome::Complete(error) = run_reflection_test(&assembler, &bad, host).unwrap() else {
        panic!("reflection eval should catch the nested update error")
    };
    let error_text = assembler.get(&error, "msg.text").unwrap();
    assert!(
        String::from_utf8_lossy(&assembler.to_binary(&error_text).unwrap())
            .contains("non-dictionary")
    );
}

#[test]
fn only_heap_reads_make_later_failure_retryable() {
    let (assembler, heap_effect) =
        compile_effect(".heap.get ['answer] >>= (\\answer -> (answer == \"ready\") =>> .r answer)");
    let ready_heap = public_record(&assembler, [("answer", assembler.values().text("ready"))]);
    let heap_host = Arc::new(TestHost::with_wake_heap(
        assembler.core_values(),
        ready_heap,
    ));
    let TaskOutcome::Complete(value) =
        run_standard_on(&assembler, &heap_effect, heap_host.clone()).unwrap()
    else {
        panic!("heap observation should retry after the heap changes")
    };
    assert_eq!(assembler.to_binary(&value).unwrap(), b"ready".as_slice());
    assert_eq!(heap_host.wait_count(), 1);

    let (local_assembler, local_effect) =
        compile_effect(".get ['answer] >>= (\\answer -> (answer == \"ready\") =>> .r answer)");
    let local_host = Arc::new(TestHost::with_wake_heap(
        local_assembler.core_values(),
        public_record(
            &local_assembler,
            [("answer", local_assembler.values().text("ready"))],
        ),
    ));
    let error = run_standard_on(&local_assembler, &local_effect, local_host.clone()).unwrap_err();
    assert!(error.to_string().contains("failed permanently"));
    assert_eq!(local_host.wait_count(), 0);
}

#[test]
fn top_level_alternative_and_unmatched_shift_are_rejected() {
    let (alternative_assembler, alternative) = compile_effect(".alt (.r 1) (.r 2)");
    assert!(
        run_standard_test(&alternative_assembler, &alternative)
            .unwrap_err()
            .to_string()
            .contains("requires an enclosing `.cut`")
    );

    let (fixpoint_assembler, fixpoint_alternative) =
        compile_effect(".fix (\\_loop -> .alt (.r 1) (.r 2))");
    assert!(
        run_standard_test(&fixpoint_assembler, &fixpoint_alternative)
            .unwrap_err()
            .to_string()
            .contains("requires an enclosing `.cut`")
    );

    let (shift_assembler, shift) =
        compile_effect(".shift \"missing\" (\\continuation -> .r continuation)");
    assert!(
        run_standard_test(&shift_assembler, &shift)
            .unwrap_err()
            .to_string()
            .contains("not in reset scope")
    );
}
