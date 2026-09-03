use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;

#[cfg(test)]
use super::DiagnosticCounts;
use super::diagnostics::DiagnosticCallback;
use super::error::net_build_error;
use super::runtime::RuntimeSharedResources;
use super::{
    Diagnostic, DiagnosticBus, DiagnosticEvent, DiagnosticSubscriber, DiagnosticSubscription,
    Error, EvaluationRuntime, NetBuilder, NetPort, PromiseResolver, ReasoningFailure,
    ReflectionInspector, RuntimeReadiness, Value, ValueEvaluator, Values,
};
use crate::compiler::{
    BinaryFileLoader, BinaryLoadArgs, CompileContext, CompileDiagnosticEmitter, ModuleLoadArgs,
    ModuleLoader, import_failure,
};
use crate::core::{
    Builtin, CoreValueFactory, Dict, EvaluationFailure, EvaluationHalt, Key, NetValue,
    PromisedValue, Value as CoreValue,
};
use crate::diagnostic::{CompilationInvocationId, CompilationTrace, Severity};
use crate::evaluation::{
    EvalContext, EvaluationSession, EvaluationSessionRun, ReflectionTaskProfile,
};
use crate::g_syntax::compile_source;
use crate::reflection::{
    CommitResult, ConflictAnalysisStrategy, HostSnapshot, ReasoningSessionId, ReflectionEffects,
    ReflectionQueryMutation, ReflectionQueryWriter, ReflectionServices, TaskCommit,
    TaskEnvironment, TaskHost, VolumeId, coordinator_task_launcher, volume_effects,
};
use crate::runtime::RuntimeValueRoot;
use crate::source::{FileSourceSystem, SourceArtifact, SourceIdentity, SourceSystem};

const GLAM_COMPATIBILITY_VERSION: &str = "0.1.0";
const IMPLEMENTATION_NAME: &str = "rust-bootstrap";

pub(super) struct AssemblerReflectionHost {
    resources: Arc<RuntimeSharedResources>,
    reasoning_session: ReasoningSessionId,
    reflection_environment: OnceLock<RuntimeValueRoot>,
    diagnostics: DiagnosticBus,
}

/// Execution resources shared by every source and recursive import in one
/// top-level module build.
///
/// Macro lookup runs in the assembler reasoning session. Macro effects and
/// explicit reflection annotations run in a separate demand session on the
/// same runtime, sharing its reflection heap while retaining their own task
/// and diagnostic state.
pub(crate) struct CompilationExecution {
    lookup: EvalContext,
    macros: EvalContext,
    _macro_owner: Arc<EvaluationSession>,
    #[cfg(test)]
    macro_host: Arc<AssemblerReflectionHost>,
    macro_diagnostics: DiagnosticBus,
    _diagnostic_forwarder: DiagnosticSubscription,
}

impl CompilationExecution {
    fn new(
        reasoning: &ReasoningSession,
        build_diagnostics: Arc<Mutex<Vec<Diagnostic>>>,
    ) -> Result<Self, Error> {
        let diagnostics = DiagnosticBus::for_runtime(&reasoning.runtime());
        let host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &reasoning.runtime(),
            diagnostics.clone(),
        ));
        host.seal_environment(reflection_environment_for_role(
            &reasoning.runtime.values(),
            &reasoning.environment(),
            "macro",
        ))?;
        let task_profile = Arc::new(ReflectionTaskProfile::sealed(coordinator_task_launcher(
            ReflectionEffects,
            host.clone(),
        )));
        let evaluation = reasoning.runtime.new_evaluation_session()?;

        let assembler_diagnostics = reasoning.diagnostics();
        let diagnostic_values = reasoning.eval_context().values().clone();
        let forwarder = diagnostics.subscribe(DiagnosticCallback(move |event: DiagnosticEvent| {
            let diagnostic = macro_reflection_diagnostic(&diagnostic_values, event.diagnostic());
            build_diagnostics
                .lock()
                .expect("build diagnostic mutex should not be poisoned")
                .push(diagnostic.clone());
            assembler_diagnostics.publish_local(diagnostic);
        }));

        Ok(Self {
            lookup: reasoning.eval_context(),
            macros: EvalContext::patient_with_task_profile(&evaluation, task_profile),
            _macro_owner: evaluation,
            #[cfg(test)]
            macro_host: host,
            macro_diagnostics: diagnostics,
            _diagnostic_forwarder: forwarder,
        })
    }

    pub(crate) fn lookup_context(&self) -> &EvalContext {
        &self.lookup
    }

    pub(crate) fn macro_context(&self) -> &EvalContext {
        &self.macros
    }

    #[cfg(test)]
    pub(crate) fn macro_diagnostic_counts(&self) -> DiagnosticCounts {
        self.macro_diagnostics.counts()
    }

    #[cfg(test)]
    pub(crate) fn macro_heap(&self) -> Value {
        self.macro_host.resources.reflection_root()
    }

    fn drain(&self) -> bool {
        let values = self.macros.values();
        let run = self.macros.run_until_quiescent();
        let (kind, report) = match run {
            EvaluationSessionRun::Complete(report) => (None, report),
            EvaluationSessionRun::Quiescent(report) => (Some("became quiescent"), report),
            EvaluationSessionRun::Deadlocked(report) => (Some("deadlocked"), report),
        };
        for (task, error) in report.failures.iter() {
            self.macro_diagnostics
                .publish_local(Diagnostic::new_with_factory(
                    values,
                    Severity::Error,
                    format!(
                        "macro reflection task {} failed: {}",
                        task.get(),
                        error.as_failure()
                    ),
                ));
        }
        if let Some(kind) = kind {
            let mut details = Vec::new();
            for task in report.unfinished {
                let dependency = task
                    .dependency
                    .map(|dependency| format!(" waiting on task {}", dependency.get()))
                    .unwrap_or_default();
                details.push(format!(
                    "task {} is {:?}{dependency}",
                    task.task.get(),
                    task.state
                ));
            }
            self.macro_diagnostics
                .publish_local(Diagnostic::new_with_factory(
                    values,
                    Severity::Error,
                    format!(
                        "macro reflection scheduler {kind} with {} unfinished task{}{}",
                        details.len(),
                        if details.len() == 1 { "" } else { "s" },
                        if details.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", details.join("; "))
                        }
                    ),
                ));
        }
        self.macro_diagnostics.counts().errors() != 0
    }

    #[cfg(test)]
    pub(crate) fn drain_for_test(&self) -> bool {
        self.drain()
    }
}

fn macro_reflection_diagnostic(values: &CoreValueFactory, diagnostic: &Diagnostic) -> Diagnostic {
    let reasoning = CoreValue::Dict(Dict::new_sync().insert(
        Key::atom_from_text("role"),
        CoreValue::Atom(crate::core::Atom::from_key(&Key::binary_from_text("macro"))),
    ));
    let origin =
        CoreValue::Dict(Dict::new_sync().insert(Key::atom_from_text("reasoning"), reasoning));
    Diagnostic::from_parts(
        values,
        diagnostic.source.clone(),
        diagnostic.severity,
        Values::from_core_factory(values.clone())
            .clone_core(&diagnostic.emission)
            .expect("forwarded macro diagnostics belong to the compilation runtime"),
        Some(origin),
    )
}

impl AssemblerReflectionHost {
    pub(super) fn new_unsealed(runtime: &EvaluationRuntime, diagnostics: DiagnosticBus) -> Self {
        let resources = runtime.shared_resources();
        Self {
            reasoning_session: resources.allocate_reasoning_session_id(),
            resources,
            reflection_environment: OnceLock::new(),
            diagnostics,
        }
    }

    fn values(&self) -> Values {
        self.resources.values()
    }

    pub(super) fn seal_environment(&self, environment: Value) -> Result<(), Error> {
        self.reflection_environment
            .set(self.resources.root_value(environment)?)
            .map_err(|_| Error::new("reflection environment was already configured"))
    }

    fn create_volume(&self, initial: Value) -> Result<(VolumeId, Value), Error> {
        let volume = self.resources.create_volume(initial)?;
        Ok((volume, volume_effects(self.resources.values.core(), volume)))
    }
}

impl TaskEnvironment for AssemblerReflectionHost {
    fn reflection_environment(&self) -> Value {
        self.reflection_environment
            .get()
            .expect("reasoning host must be sealed before it runs tasks")
            .value(self.resources.id)
    }
}

impl ReflectionServices for AssemblerReflectionHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        self.diagnostics.publish_local(diagnostic);
    }

    fn query_writer(&self) -> Option<Arc<dyn ReflectionQueryWriter>> {
        Some(self.resources.clone())
    }
}

impl ReflectionQueryWriter for RuntimeSharedResources {
    fn update_query_guarded(
        &self,
        mutation: ReflectionQueryMutation<'_>,
        handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        result: Value,
    ) -> Box<dyn FnOnce() + Send> {
        result
            .require_runtime(self.id)
            .expect("reflection query results belong to the runtime");
        let updated = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .update_query(handle, result);
        assert!(
            updated,
            "task status query must remain in its runtime domain"
        );

        let epoch = self.observations.advance();
        let work = self.work.upgrade();
        let scheduler_changed = work
            .as_ref()
            .map(|work| work.publish_runtime_observation_guarded(mutation.guard(), epoch));
        let observations = self.observations.clone();
        Box::new(move || {
            observations.notify_all();
            if let (Some(work), Some(changed)) = (work, scheduler_changed) {
                work.notify_runtime_observation(changed);
            }
        })
    }
}

impl TaskHost<ReflectionEffects> for AssemblerReflectionHost {
    fn snapshot(&self) -> HostSnapshot<ReflectionEffects> {
        let (generation, store) = self.resources.reflection_snapshot();
        HostSnapshot::new(generation, store, ())
    }

    fn commit(&self, commit: TaskCommit<ReflectionEffects>) -> CommitResult {
        let (store, _extra_snapshot, extra) = commit.into_parts();
        match self.resources.commit_reflection(&store) {
            crate::reflection::StoreCommitResult::Committed => {}
            crate::reflection::StoreCommitResult::Conflict => {
                return CommitResult::Conflict;
            }
            crate::reflection::StoreCommitResult::MissingVolume(volume) => {
                return CommitResult::MissingVolume(volume);
            }
        }
        let diagnostics = extra.diagnostics().to_vec();
        for diagnostic in diagnostics {
            self.diagnostics.publish_local(diagnostic);
        }
        extra.commit_updates();
        CommitResult::Committed
    }

    fn reasoning_session_id(&self) -> Option<ReasoningSessionId> {
        Some(self.reasoning_session)
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        self.resources.wait_for_change(observed_generation)
    }
}

#[derive(Clone)]
pub(super) struct ReasoningSession {
    host: Arc<AssemblerReflectionHost>,
    task_profile: Arc<ReflectionTaskProfile>,
    diagnostics: DiagnosticBus,
    pub(super) runtime: EvaluationRuntime,
    evaluation: Arc<EvaluationSession>,
}

impl ReasoningSession {
    fn from_host(
        host: Arc<AssemblerReflectionHost>,
        diagnostics: DiagnosticBus,
        runtime: EvaluationRuntime,
    ) -> Result<Self, Error> {
        let task_profile = Arc::new(ReflectionTaskProfile::sealed(coordinator_task_launcher(
            ReflectionEffects,
            host.clone(),
        )));
        let evaluation = runtime.new_evaluation_session()?;
        Ok(Self {
            host,
            task_profile,
            diagnostics,
            runtime,
            evaluation,
        })
    }

    fn environment(&self) -> Value {
        self.host.reflection_environment()
    }

    fn diagnostics(&self) -> DiagnosticBus {
        self.diagnostics.clone()
    }

    fn runtime(&self) -> EvaluationRuntime {
        self.runtime.clone()
    }

    fn eval_context(&self) -> EvalContext {
        EvalContext::patient_with_task_profile(&self.evaluation, self.task_profile.clone())
    }

    fn conflict_analysis(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.runtime.conflict_analysis()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleInput {
    File(PathBuf),
    Script { extension: String, body: Bytes },
}

impl ModuleInput {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    pub fn script(extension: impl Into<String>, body: impl Into<String>) -> Self {
        Self::Script {
            extension: extension.into(),
            body: Bytes::from(body.into()),
        }
    }
}

struct PreparedSource {
    source: Arc<SourceArtifact>,
    context: CompileContext,
    had_errors: Arc<AtomicBool>,
}

struct CompileSetup {
    module_path: Arc<[String]>,
    prior_defs: RuntimeValueRoot,
    final_defs: RuntimeValueRoot,
    module_loader: ModuleLoader,
    binary_loader: BinaryFileLoader,
    session: Arc<Mutex<Vec<Diagnostic>>>,
    execution: Arc<CompilationExecution>,
}

#[derive(Debug, Clone)]
pub struct BuiltModule {
    value: Value,
    diagnostics: Vec<Diagnostic>,
}

impl BuiltModule {
    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

pub(super) fn authoritative_reflection_environment(
    values: &Values,
    environment: Value,
    role: &str,
) -> Result<(Value, bool), Error> {
    let CoreValue::Dict(root) = values.clone_core(&environment)? else {
        return Err(Error::new("reflection environment must be a dictionary"));
    };
    let glam_key = Key::atom_from_text("glam");
    let replaced_glam = root.get(&glam_key).is_some();
    Ok((
        values.wrap(CoreValue::Dict(
            root.insert(glam_key, authoritative_glam_environment(role)),
        )),
        replaced_glam,
    ))
}

fn authoritative_glam_environment(role: &str) -> CoreValue {
    let implementation = Dict::new_sync()
        .insert(
            Key::atom_from_text("name"),
            CoreValue::binary_from_text(IMPLEMENTATION_NAME),
        )
        .insert(
            Key::atom_from_text("version"),
            CoreValue::binary_from_text(env!("CARGO_PKG_VERSION")),
        );
    let glam = Dict::new_sync()
        .insert(
            Key::atom_from_text("version"),
            CoreValue::binary_from_text(GLAM_COMPATIBILITY_VERSION),
        )
        .insert(
            Key::atom_from_text("implementation"),
            CoreValue::Dict(implementation),
        )
        .insert(
            Key::atom_from_text("reasoning"),
            CoreValue::Dict(Dict::new_sync().insert(
                Key::atom_from_text("role"),
                CoreValue::Atom(crate::core::Atom::from_key(&Key::binary_from_text(role))),
            )),
        )
        .insert(
            Key::atom_from_text("origin"),
            CoreValue::Dict(Dict::new_sync().insert(
                Key::atom_from_text("inspect"),
                CoreValue::Builtin(Builtin::InspectOrigin),
            )),
        );
    CoreValue::Dict(glam)
}

fn reflection_environment_for_role(values: &Values, environment: &Value, role: &str) -> Value {
    let CoreValue::Dict(root) = values
        .clone_core(environment)
        .expect("authoritative reflection environment belongs to its runtime")
    else {
        unreachable!("authoritative reflection environment must be a dictionary")
    };
    values.wrap(CoreValue::Dict(root.insert(
        Key::atom_from_text("glam"),
        authoritative_glam_environment(role),
    )))
}

/// Owner handle for one protected volume in an evaluation runtime.
///
/// Capability values may be cloned freely, but only this handle can remove the
/// volume and recover its final unforced value. Dropping the handle does not
/// revoke the volume.
pub struct ReasoningVolume {
    resources: Arc<RuntimeSharedResources>,
    volume: VolumeId,
    effects: Value,
}

impl ReasoningVolume {
    /// Returns the closed `{get, set, rewrite}` effect capability value.
    pub fn effects(&self) -> Value {
        debug_assert_eq!(self.effects.runtime_id(), self.resources.id);
        self.effects.clone()
    }

    /// Removes the volume and returns its final value without forcing it.
    /// Further uses of any capability for this volume produce
    /// use-after-revoke errors.
    pub fn revoke(self) -> Result<Value, Error> {
        self.resources.revoke_volume(self.volume)
    }
}
#[derive(Clone)]
pub struct Assembler {
    source_system: Arc<dyn SourceSystem>,
    next_compilation_invocation: Arc<AtomicU64>,
    pub(super) reasoning: ReasoningSession,
    diagnostic_attachments: Vec<DiagnosticAttachment>,
}

#[derive(Clone)]
struct DiagnosticAttachment {
    _subscription: DiagnosticSubscription,
}

/// Staged construction of one assembler and its single reasoning session.
pub struct AssemblerBuilder {
    source_system: Arc<dyn SourceSystem>,
    pub(super) runtime: EvaluationRuntime,
    host: Arc<AssemblerReflectionHost>,
    diagnostics: DiagnosticBus,
    reflection_environment: Option<Value>,
    diagnostic_attachments: Vec<DiagnosticAttachment>,
    pending_diagnostics: Vec<Diagnostic>,
    runtime_locked: bool,
    runtime_supplied: bool,
    conflict_analysis_requested: bool,
    construction_error: Option<Arc<str>>,
}

/// Capabilities available while constructing the immutable reflection
/// environment. The borrow cannot escape the construction closure.
pub struct ReflectionEnvironmentBuilder<'a> {
    host: &'a Arc<AssemblerReflectionHost>,
}

impl ReflectionEnvironmentBuilder<'_> {
    /// Returns the selected runtime's value-construction service.
    pub fn values(&self) -> Values {
        self.host.values()
    }

    /// Creates a protected volume belonging to the selected evaluation runtime.
    pub fn create_volume(&mut self, initial: Value) -> Result<ReasoningVolume, Error> {
        create_reasoning_volume(self.host, initial)
    }

    /// Creates a promised environment value and its affine host resolver.
    /// Same-runtime work subscribes directly when it blocks on the value, so
    /// the resolver needs no later assembler-specific arming step.
    pub fn promise(&mut self, label: impl Into<Arc<str>>) -> (Value, PromiseResolver) {
        let values = self.host.values();
        let promise = PromisedValue::new(&values.core, label);
        (
            values.wrap(CoreValue::Promised(promise.clone())),
            PromiseResolver {
                runtime: self.host.resources.id,
                promise: Some(promise),
            },
        )
    }
}

impl Default for AssemblerBuilder {
    fn default() -> Self {
        let diagnostics = DiagnosticBus::new();
        let runtime = EvaluationRuntime::new(0)
            .expect("zero-worker evaluation runtime must be constructible");
        let host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &runtime,
            diagnostics.clone(),
        ));
        Self {
            source_system: Arc::new(FileSourceSystem::default()),
            runtime,
            host,
            diagnostics,
            reflection_environment: None,
            diagnostic_attachments: Vec::new(),
            pending_diagnostics: Vec::new(),
            runtime_locked: false,
            runtime_supplied: false,
            conflict_analysis_requested: false,
            construction_error: None,
        }
    }
}

impl AssemblerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn source_system(mut self, source_system: impl SourceSystem + 'static) -> Self {
        self.source_system = Arc::new(source_system);
        self
    }

    pub fn evaluation_runtime(mut self, runtime: EvaluationRuntime) -> Self {
        if self.runtime_locked {
            self.record_construction_error(
                "the evaluation runtime must be selected before creating protected volumes or the reflection environment",
            );
            return self;
        }
        if self.conflict_analysis_requested {
            self.record_construction_error(
                "an attached evaluation runtime already owns its conflict-analysis strategy",
            );
            return self;
        }
        self.runtime = runtime;
        self.host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &self.runtime,
            self.diagnostics.clone(),
        ));
        self.runtime_supplied = true;
        self
    }

    /// Selects the diagnostic bus used by this assembler.
    ///
    /// This is a construction-time boundary because the reflection host keeps
    /// the bus as part of its immutable task profile.
    pub fn diagnostic_bus(mut self, diagnostics: DiagnosticBus) -> Self {
        if self.runtime_locked || !self.diagnostic_attachments.is_empty() {
            self.record_construction_error(
                "the diagnostic bus must be selected before reflection environment construction or subscriber attachment",
            );
            return self;
        }
        self.diagnostics = diagnostics;
        self.host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &self.runtime,
            self.diagnostics.clone(),
        ));
        self
    }

    pub fn conflict_analysis(mut self, strategy: Arc<dyn ConflictAnalysisStrategy>) -> Self {
        if self.runtime_locked {
            self.record_construction_error(
                "the conflict-analysis strategy must be selected before creating protected volumes or the reflection environment",
            );
            return self;
        }
        if self.runtime_supplied {
            self.record_construction_error(
                "an attached evaluation runtime already owns its conflict-analysis strategy",
            );
            return self;
        }
        match EvaluationRuntime::with_conflict_analysis(self.runtime.worker_threads(), strategy) {
            Ok(runtime) => {
                self.runtime = runtime;
                self.host = Arc::new(AssemblerReflectionHost::new_unsealed(
                    &self.runtime,
                    self.diagnostics.clone(),
                ));
                self.conflict_analysis_requested = true;
            }
            Err(error) => self.record_construction_error(error.to_string()),
        }
        self
    }

    fn record_construction_error(&mut self, message: impl Into<Arc<str>>) {
        if self.construction_error.is_none() {
            self.construction_error = Some(message.into());
        }
    }

    pub fn diagnostic_subscriber(
        mut self,
        subscriber: impl DiagnosticSubscriber + 'static,
    ) -> Self {
        let subscriber: Arc<dyn DiagnosticSubscriber> = Arc::new(subscriber);
        let subscription = self.diagnostics.subscribe_shared(subscriber.clone());
        self.diagnostic_attachments.push(DiagnosticAttachment {
            _subscription: subscription,
        });
        self
    }

    pub fn diagnostic_callback<F>(self, callback: F) -> Self
    where
        F: Fn(DiagnosticEvent) + Send + Sync + 'static,
    {
        self.diagnostic_subscriber(DiagnosticCallback(callback))
    }

    pub fn create_volume(&mut self, initial: Value) -> Result<ReasoningVolume, Error> {
        self.runtime_locked = true;
        create_reasoning_volume(&self.host, initial)
    }

    /// Constructs the client portion of the reflection environment. The
    /// closure may create session-bound protected volumes before the session
    /// becomes runnable.
    pub fn reflection_environment<F>(mut self, build: F) -> Result<Self, Error>
    where
        F: FnOnce(&mut ReflectionEnvironmentBuilder<'_>) -> Result<Value, Error>,
    {
        if self.reflection_environment.is_some() {
            return Err(Error::new("reflection environment was already configured"));
        }
        self.runtime_locked = true;
        let environment = build(&mut ReflectionEnvironmentBuilder { host: &self.host })?;
        environment.require_runtime(self.runtime.id())?;
        let values = self.runtime.values();
        let (environment, replaced_glam) =
            authoritative_reflection_environment(&values, environment, "assembler")?;
        self.reflection_environment = Some(environment);
        if replaced_glam {
            self.pending_diagnostics.push(Diagnostic::new(
                &self.runtime.values(),
                Severity::Warning,
                "reflection environment namespace `glam` is reserved; supplied value was ignored",
            ));
        }
        Ok(self)
    }

    pub fn build(mut self) -> Result<Assembler, Error> {
        if let Some(error) = self.construction_error.take() {
            return Err(Error::new(error));
        }
        self.diagnostics.bind_runtime(&self.runtime)?;
        let environment = match self.reflection_environment.take() {
            Some(environment) => environment,
            None => {
                authoritative_reflection_environment(
                    &self.runtime.values(),
                    self.runtime.values().empty_dict(),
                    "assembler",
                )?
                .0
            }
        };
        self.host.seal_environment(environment)?;
        if !self.runtime.has_default_reflection_profile() {
            match self
                .runtime
                .seal_default_reflection_profile(coordinator_task_launcher(
                    ReflectionEffects,
                    self.host.clone(),
                )) {
                Ok(()) => {}
                Err(error) if self.runtime.has_default_reflection_profile() => {
                    // Another builder sealed the same dormant runtime first.
                    // This assembler reuses that immutable default profile.
                    drop(error);
                }
                Err(error) => return Err(error),
            }
        }
        let reasoning =
            ReasoningSession::from_host(self.host, self.diagnostics.clone(), self.runtime)?;
        for diagnostic in self.pending_diagnostics {
            self.diagnostics.publish_local(diagnostic);
        }
        Ok(Assembler {
            source_system: self.source_system,
            next_compilation_invocation: Arc::new(AtomicU64::new(1)),
            reasoning,
            diagnostic_attachments: self.diagnostic_attachments,
        })
    }
}

fn create_reasoning_volume(
    host: &Arc<AssemblerReflectionHost>,
    initial: Value,
) -> Result<ReasoningVolume, Error> {
    let (volume, effects) = host.create_volume(initial)?;
    Ok(ReasoningVolume {
        resources: host.resources.clone(),
        volume,
        effects,
    })
}

impl Default for Assembler {
    fn default() -> Self {
        AssemblerBuilder::default()
            .build()
            .expect("the default assembler must be constructible")
    }
}

impl Assembler {
    pub fn builder() -> AssemblerBuilder {
        AssemblerBuilder::new()
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Returns this assembler's privileged reflection-inspection facade.
    ///
    /// Executable and IDE clients are reflection observers, not ordinary Glam
    /// programs. Client policy that needs to inspect opaque value structure
    /// belongs behind this explicit boundary rather than in evaluator
    /// builtins.
    pub fn reflection(&self) -> ReflectionInspector<'_> {
        ReflectionInspector { assembler: self }
    }

    /// Returns this assembler's ordinary semantic demand facade.
    pub fn evaluator(&self) -> ValueEvaluator<'_> {
        ValueEvaluator { assembler: self }
    }

    #[cfg(test)]
    pub(crate) fn test_compilation_execution(&self) -> Arc<CompilationExecution> {
        Arc::new(
            CompilationExecution::new(&self.reasoning, Arc::new(Mutex::new(Vec::new())))
                .expect("test compilation execution must be constructible"),
        )
    }

    #[cfg(test)]
    pub(crate) fn test_reflection_heap(&self) -> Value {
        self.reasoning.runtime.reflection_root()
    }

    /// Creates a host-resolved promised value and its unique resolver.
    ///
    /// Resolving, failing, or dropping the resolver wakes each same-runtime
    /// work item currently blocked on the unresolved value.
    pub fn promise(&self, label: impl Into<Arc<str>>) -> (Value, PromiseResolver) {
        let promise = PromisedValue::new(self.eval_context().values(), label);
        let values = self.values();
        (
            values.wrap(CoreValue::Promised(promise.clone())),
            PromiseResolver {
                runtime: self.reasoning.runtime.id(),
                promise: Some(promise),
            },
        )
    }

    /// Creates a protected reflection volume initialized with `initial`.
    /// Possession of the returned Glam capability value is the authority to
    /// access it from any reasoning session in this runtime; ordinary
    /// `.heap.*` requests cannot address the volume.
    pub fn create_volume(&self, initial: Value) -> Result<ReasoningVolume, Error> {
        create_reasoning_volume(&self.reasoning.host, initial)
    }

    /// Returns the cached closed Glam function used by the executable's
    /// default terminal logger. It expects an enriched diagnostic containing
    /// the conventional `msg` and `viewer` fields, including the observer's
    /// complete textual `viewer.header`, and returns bytes.
    pub fn default_diagnostic_formatter(&self) -> Value {
        let values = self.core_values();
        self.values()
            .wrap(crate::g_syntax::default_diagnostic_formatter(&values))
    }

    /// Returns the read-only environment shared by reflection tasks in this
    /// assembler's evaluation session.
    pub fn reflection_environment(&self) -> Value {
        self.reasoning.environment()
    }

    /// Returns this session environment with another authoritative reasoning
    /// role. Service sessions retain the client-provided environment while
    /// identifying themselves independently from the assembler session.
    pub fn reflection_environment_for_role(&self, role: impl AsRef<str>) -> Value {
        reflection_environment_for_role(
            &self.values(),
            &self.reasoning.environment(),
            role.as_ref(),
        )
    }

    /// Returns the shared execution resources used by this assembler and any
    /// service evaluation sessions explicitly attached to it.
    pub fn evaluation_runtime(&self) -> EvaluationRuntime {
        self.reasoning.runtime()
    }

    /// Returns this assembler runtime's explicit value-construction service.
    pub fn values(&self) -> Values {
        self.reasoning.runtime.values()
    }

    /// Returns the read-conflict strategy fixed for this reasoning session.
    pub fn conflict_analysis(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.reasoning.conflict_analysis()
    }

    /// Returns this reasoning session's non-buffering diagnostic bus.
    pub fn diagnostic_bus(&self) -> DiagnosticBus {
        self.reasoning.diagnostics()
    }

    pub(crate) fn eval_context(&self) -> EvalContext {
        self.reasoning.eval_context()
    }

    pub(crate) fn core_values(&self) -> CoreValueFactory {
        self.eval_context().values().clone()
    }

    /// Pumps useful work across every evaluation session in this assembler's
    /// runtime until it reaches a stable instant, then returns an
    /// observational readiness snapshot.
    ///
    /// This method imposes no step or time limit. A runnable infinite task
    /// therefore keeps it running forever. The returned snapshot is not
    /// committed: clients explicitly settle ready exit votes or choose
    /// whether to kill a deadlock.
    pub fn drain_reasoning(&self) -> RuntimeReadiness {
        let runtime = self.evaluation_runtime();
        runtime.pump_until_stable();
        runtime.readiness()
    }

    /// Acknowledges a failed task retained by a settled
    /// [`QuiescenceReport`].
    ///
    /// Acknowledgement removes the failure from later reasoning reports but
    /// does not change the task's terminal result. Repeated acknowledgement is
    /// harmless. Any assembler view of the same evaluation runtime may
    /// acknowledge the producer's failure; a report from another runtime is
    /// rejected.
    pub fn acknowledge_reasoning_failure(&self, failure: &ReasoningFailure) -> Result<(), Error> {
        let context = self.eval_context();
        if context.values().runtime_id() != failure.runtime {
            return Err(Error::new(
                "reasoning failure belongs to a different evaluation runtime",
            ));
        }
        context.acknowledge_task_failure(failure.session, failure.task);
        Ok(())
    }

    /// Installs another retained diagnostic subscription
    /// without rebuilding or otherwise disturbing its reasoning session.
    pub fn with_diagnostic_subscriber(
        mut self,
        subscriber: impl DiagnosticSubscriber + 'static,
    ) -> Self {
        let subscriber: Arc<dyn DiagnosticSubscriber> = Arc::new(subscriber);
        let subscription = self
            .reasoning
            .diagnostics()
            .subscribe_shared(subscriber.clone());
        self.diagnostic_attachments.push(DiagnosticAttachment {
            _subscription: subscription,
        });
        self
    }

    pub fn with_diagnostic_callback<F>(self, callback: F) -> Self
    where
        F: Fn(DiagnosticEvent) + Send + Sync + 'static,
    {
        self.with_diagnostic_subscriber(DiagnosticCallback(callback))
    }

    pub fn module<I, S>(&self, module_path: I) -> ModuleBuilder<'_>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ModuleBuilder {
            assembler: self,
            module_path: Arc::from(
                module_path
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ),
            inputs: Vec::new(),
            initial_definitions: self.values().empty_dict(),
        }
    }

    pub(crate) fn record_diagnostic(&self, diagnostic: Diagnostic) {
        self.reasoning.diagnostics().publish_local(diagnostic);
    }

    fn next_compilation_invocation(&self) -> CompilationInvocationId {
        let id = self
            .next_compilation_invocation
            .fetch_add(1, Ordering::Relaxed);
        assert!(id != u64::MAX, "compilation invocation IDs exhausted");
        CompilationInvocationId::new(id)
    }

    pub(super) fn evaluation_error(&self, error: EvaluationHalt) -> Error {
        Error::from_eval(&self.core_values(), error)
    }

    /// Builds one closed interaction-net value through a checked, effect-style
    /// API. The callback's returned port becomes the sole exposed port.
    pub fn net(
        &self,
        build: impl for<'net> FnOnce(&mut NetBuilder<'net>) -> Result<NetPort<'net>, Error>,
    ) -> Result<Value, Error> {
        let mut builder = NetBuilder::new(self.values());
        let exposed = build(&mut builder)?.port;
        let template = builder
            .builder
            .try_finish(exposed)
            .map_err(net_build_error)?;
        let values = self.values();
        Ok(values.wrap(CoreValue::Net(NetValue::new(
            values.core().instantiate_core_net(&template),
        ))))
    }

    // TODO: add reflection snapshots and event subscriptions here. Reflection
    // producers should feed the same bounded history rather than print.

    fn build_module(
        &self,
        module_path: Arc<[String]>,
        inputs: Vec<ModuleInput>,
        initial_definitions: Value,
    ) -> Result<BuiltModule, Error> {
        initial_definitions.require_runtime(self.reasoning.runtime.id())?;
        let session = Arc::new(Mutex::new(Vec::new()));
        let execution = Arc::new(CompilationExecution::new(&self.reasoning, session.clone())?);
        let result = self.build_module_inner(
            module_path,
            inputs,
            initial_definitions.into_runtime_root(),
            session.clone(),
            execution.clone(),
        );
        let execution_failed = execution.drain();
        let diagnostics = session
            .lock()
            .expect("build diagnostic mutex should not be poisoned")
            .clone();

        match (result, execution_failed) {
            (Ok(_), true) => {
                Err(Error::new("module macro reasoning failed").with_diagnostics(diagnostics))
            }
            (Ok(value), false) => Ok(BuiltModule {
                value: Value::from_runtime_root(value),
                diagnostics,
            }),
            (Err(error), _) => Err(error.with_diagnostics(diagnostics)),
        }
    }

    fn build_module_inner(
        &self,
        module_path: Arc<[String]>,
        inputs: Vec<ModuleInput>,
        mut definitions: RuntimeValueRoot,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        execution: Arc<CompilationExecution>,
    ) -> Result<RuntimeValueRoot, Error> {
        let module_loader = self.module_loader(session.clone(), execution.clone());
        let binary_loader = self.binary_loader();
        let module_context = CompileContext::from_module_path_with_values(
            self.core_values(),
            module_path.iter().cloned(),
        )
        .with_local_module_loader(module_loader.clone())
        .with_local_binary_loader(binary_loader.clone());
        let final_defs = module_context.final_defs_root().clone();
        let mut had_errors = false;

        for input in inputs.iter().rev() {
            let prepared = self.prepare_input(
                input,
                CompileSetup {
                    module_path: module_path.clone(),
                    prior_defs: definitions.clone(),
                    final_defs: final_defs.clone(),
                    module_loader: module_loader.clone(),
                    binary_loader: binary_loader.clone(),
                    session: session.clone(),
                    execution: execution.clone(),
                },
            )?;
            definitions = RuntimeValueRoot::new(
                &self.core_values(),
                compile_source(prepared.source.bytes(), &prepared.context),
            );
            had_errors |= prepared.had_errors.load(Ordering::Relaxed);
        }

        if had_errors {
            return Err(Error::new("module failed to compile"));
        }

        self.seal_module(&module_context, &definitions)
    }

    fn prepare_input(
        &self,
        input: &ModuleInput,
        setup: CompileSetup,
    ) -> Result<PreparedSource, Error> {
        let CompileSetup {
            module_path,
            prior_defs,
            final_defs,
            module_loader,
            binary_loader,
            session,
            execution,
        } = setup;
        match input {
            ModuleInput::File(path) => {
                let source = Arc::new(
                    self.source_system
                        .load_top_level(path)
                        .map_err(|error| Error::new(error.to_string()))?,
                );
                let trace = Arc::new(CompilationTrace::root(
                    self.next_compilation_invocation(),
                    &source,
                    module_path.clone(),
                ));
                let had_errors = Arc::new(AtomicBool::new(false));
                let context = CompileContext::from_module_path_with_values(
                    self.core_values(),
                    module_path.iter().cloned(),
                )
                .with_importer_source(source.clone())
                .with_compilation_trace(trace.clone())
                .with_prior_defs_root(prior_defs)
                .with_final_defs_root(final_defs)
                .with_local_module_loader(module_loader)
                .with_local_binary_loader(binary_loader)
                .with_compilation_execution(execution)
                .with_diagnostic_emitter(self.compile_diagnostic_emitter(
                    trace,
                    session,
                    had_errors.clone(),
                ));
                Ok(PreparedSource {
                    source,
                    context,
                    had_errors,
                })
            }
            ModuleInput::Script { extension, body } => {
                let label: Arc<str> = Arc::from(format!("<script.{extension}>"));
                let source = Arc::new(SourceArtifact::new(
                    body.clone(),
                    SourceIdentity::script(label, body.clone()),
                ));
                let trace = Arc::new(CompilationTrace::root(
                    self.next_compilation_invocation(),
                    &source,
                    module_path.clone(),
                ));
                let had_errors = Arc::new(AtomicBool::new(false));
                let context = CompileContext::from_module_path_with_values(
                    self.core_values(),
                    module_path.iter().cloned(),
                )
                .with_compilation_trace(trace.clone())
                .with_prior_defs_root(prior_defs)
                .with_final_defs_root(final_defs)
                .with_local_module_loader(module_loader)
                .with_local_binary_loader(binary_loader)
                .with_compilation_execution(execution)
                .with_diagnostic_emitter(self.compile_diagnostic_emitter(
                    trace,
                    session,
                    had_errors.clone(),
                ));
                Ok(PreparedSource {
                    source,
                    context,
                    had_errors,
                })
            }
        }
    }

    fn module_loader(
        &self,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        execution: Arc<CompilationExecution>,
    ) -> ModuleLoader {
        let assembler = self.clone();
        Arc::new(move |args| assembler.load_local_module(args, session.clone(), execution.clone()))
    }

    fn binary_loader(&self) -> BinaryFileLoader {
        let assembler = self.clone();
        Arc::new(move |args| assembler.load_local_binary(args))
    }

    fn load_local_module(
        &self,
        args: ModuleLoadArgs,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        execution: Arc<CompilationExecution>,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let importer = args.importer_source.as_ref().ok_or_else(|| {
            import_failure(
                format!(
                    "local import `{}` cannot be loaded from a source without an import resolver",
                    args.request.as_str()
                ),
                args.request.as_str(),
                args.importer_trace.as_deref(),
                None,
            )
        })?;
        let source = Arc::new(importer.load_relative(&args.request).map_err(|error| {
            import_failure(
                format!(
                    "local import `{}` could not be loaded: {error}",
                    args.request.as_str()
                ),
                args.request.as_str(),
                args.importer_trace.as_deref(),
                Some(importer),
            )
        })?);
        let module_loader = self.module_loader(session.clone(), execution.clone());
        let binary_loader = self.binary_loader();
        let had_errors = Arc::new(AtomicBool::new(false));
        let trace = match args.importer_trace {
            Some(parent) => Arc::new(CompilationTrace::imported(
                self.next_compilation_invocation(),
                &source,
                args.module_path.clone(),
                parent,
                Arc::from(args.request.as_str()),
                args.extends.clone(),
            )),
            None => Arc::new(CompilationTrace::root(
                self.next_compilation_invocation(),
                &source,
                args.module_path.clone(),
            )),
        };
        let context = CompileContext::from_module_path_with_values(
            self.core_values(),
            args.module_path.iter().cloned(),
        )
        .with_importer_source(source.clone())
        .with_compilation_trace(trace.clone())
        .with_prior_defs_root(args.prior_defs)
        .with_final_defs_root(args.final_defs)
        .with_local_module_loader(module_loader)
        .with_local_binary_loader(binary_loader)
        .with_compilation_execution(execution)
        .with_diagnostic_emitter(self.compile_diagnostic_emitter(
            trace.clone(),
            session,
            had_errors.clone(),
        ));
        let definitions = compile_source(source.bytes(), &context);

        if had_errors.load(Ordering::Relaxed) {
            Err(import_failure(
                format!(
                    "local import `{}` failed to compile",
                    source.identity().label()
                ),
                args.request.as_str(),
                Some(&trace),
                Some(&source),
            ))
        } else {
            Ok(RuntimeValueRoot::new(&self.core_values(), definitions))
        }
    }

    fn load_local_binary(
        &self,
        args: BinaryLoadArgs,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let importer = args.importer_source.as_ref().ok_or_else(|| {
            import_failure(
                format!(
                    "binary import `{}` cannot be loaded from a source without an import resolver",
                    args.request.as_str()
                ),
                args.request.as_str(),
                args.importer_trace.as_deref(),
                None,
            )
        })?;
        importer
            .load_relative(&args.request)
            .map(|artifact| {
                RuntimeValueRoot::new(
                    &self.core_values(),
                    CoreValue::Binary(artifact.bytes().clone()),
                )
            })
            .map_err(|error| {
                import_failure(
                    format!(
                        "binary import `{}` could not be loaded: {error}",
                        args.request.as_str()
                    ),
                    args.request.as_str(),
                    args.importer_trace.as_deref(),
                    Some(importer),
                )
            })
    }

    fn seal_module(
        &self,
        context: &CompileContext,
        definitions: &RuntimeValueRoot,
    ) -> Result<RuntimeValueRoot, Error> {
        let module_value = self.core_values().with_runtime_value_access(|_| {
            let CoreValue::Promised(final_defs) = context.final_defs() else {
                panic!("CompileContext.final_defs must be a promised value");
            };
            final_defs
                .set(definitions.as_core().clone())
                .expect("CompileContext.final_defs future must be unassigned");
            definitions.as_core().clone()
        });
        self.eval_context()
            .evaluate_whnf(&module_value)
            .map(|value| RuntimeValueRoot::new(&self.core_values(), value))
            .map_err(|error| self.evaluation_error(error))
    }

    fn compile_diagnostic_emitter(
        &self,
        trace: Arc<CompilationTrace>,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        had_errors: Arc<AtomicBool>,
    ) -> CompileDiagnosticEmitter {
        let assembler = self.clone();
        Arc::new(move |severity, message| {
            if severity == Severity::Error {
                had_errors.store(true, Ordering::Relaxed);
            }
            let diagnostic =
                Diagnostic::from_compile(assembler.values().core(), &trace, severity, message);
            session
                .lock()
                .expect("build diagnostic mutex should not be poisoned")
                .push(diagnostic.clone());
            assembler.record_diagnostic(diagnostic);
        })
    }
}

pub struct ModuleBuilder<'a> {
    assembler: &'a Assembler,
    module_path: Arc<[String]>,
    inputs: Vec<ModuleInput>,
    initial_definitions: Value,
}

impl ModuleBuilder<'_> {
    pub fn input(mut self, input: ModuleInput) -> Self {
        self.inputs.push(input);
        self
    }

    pub fn inputs(mut self, inputs: impl IntoIterator<Item = ModuleInput>) -> Self {
        self.inputs.extend(inputs);
        self
    }

    pub fn file(self, path: impl Into<PathBuf>) -> Self {
        self.input(ModuleInput::file(path))
    }

    pub fn script(self, extension: impl Into<String>, body: impl Into<String>) -> Self {
        self.input(ModuleInput::script(extension, body))
    }

    pub fn initial_definitions(mut self, definitions: Value) -> Self {
        self.initial_definitions = definitions;
        self
    }

    pub fn build(self) -> Result<BuiltModule, Error> {
        self.assembler
            .build_module(self.module_path, self.inputs, self.initial_definitions)
    }
}
