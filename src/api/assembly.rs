use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use bytes::Bytes;

#[cfg(test)]
use super::DiagnosticCounts;
use super::diagnostics::DiagnosticCallback;
use super::error::net_build_error;
use super::runtime::{RuntimeSharedResources, admit_runtime_request};
use super::turn::environment::{REFL, committed_environment};
use super::{
    Diagnostic, DiagnosticBus, DiagnosticEvent, DiagnosticSubscriber, DiagnosticSubscription,
    Error, EvaluationRuntime, NetBuilder, NetPort, PromiseResolver, ReasoningFailure,
    ReflectionInspector, RuntimeReadiness, Value, ValueEvaluator, Values,
};
use crate::compiler::{
    BinaryFileLoader, BinaryLoadArgs, CompileCall, CompileContext, CompileDiagnosticEmitter,
    ImportCompilers, ImportSite, ModuleLoadArgs, ModuleLoader, ServedImports, SourceCache,
    SourceCompiler, SourceRecord, SourceRequester, import_failure,
};
use crate::core::{
    Builtin, BuiltinCall, CoreValueFactory, Dict, EvaluationFailure, EvaluationHalt, Key,
    LazyValue, ManagedPromiseRoot, NetValue, PromisedValue, Value as CoreValue, keys,
};
use crate::diagnostic::{CompilationInvocationId, CompilationTrace, Severity};
use crate::evaluation::{EvalContext, EvaluationSession, ReflectionTaskProfile};
use crate::g_syntax::compile_source;
use crate::number::Number;
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
    #[cfg(test)]
    macro_diagnostics: DiagnosticBus,
    _diagnostic_forwarder: DiagnosticSubscription,
}

impl CompilationExecution {
    fn new(reasoning: &ReasoningSession) -> Result<Self, Error> {
        let diagnostics = DiagnosticBus::for_runtime(&reasoning.runtime());
        let host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &reasoning.runtime(),
            diagnostics.clone(),
        ));
        host.seal_environment(reflection_environment_for_role(
            &reasoning.runtime.values(),
            &reasoning.sealed_environment(),
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
            assembler_diagnostics.publish_local(diagnostic);
        }));

        Ok(Self {
            lookup: reasoning.eval_context(),
            macros: EvalContext::patient_with_task_profile(&evaluation, task_profile),
            _macro_owner: evaluation,
            #[cfg(test)]
            macro_host: host,
            #[cfg(test)]
            macro_diagnostics: diagnostics,
            _diagnostic_forwarder: forwarder,
        })
    }

    /// The pending reply that a compile-time evaluation halted on, where it
    /// halted on one: a compile waits for it, then runs again
    /// (`compile-yields-an-open-mixin`).
    pub(crate) fn awaited_reply(&self, halt: &EvaluationHalt) -> Option<ManagedPromiseRoot> {
        if let Some(promise) = halt.unassigned_promise_root() {
            return Some(promise.clone());
        }
        let wait = halt.blocked_on()?;
        self.awaited_dependency(&crate::evaluation::WorkDependency::Wait(wait.0))
    }

    /// The pending reply that `dependency` ultimately waits on.
    pub(crate) fn awaited_dependency(
        &self,
        dependency: &crate::evaluation::WorkDependency,
    ) -> Option<ManagedPromiseRoot> {
        self.lookup
            .values()
            .work_coordinator()?
            .awaited_promise_root(dependency)
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
}

fn macro_reflection_diagnostic(values: &CoreValueFactory, diagnostic: &Diagnostic) -> Diagnostic {
    let public_values = Values::from_core_factory(values.clone());
    let origin = public_values.with_access(|access| {
        let reasoning = CoreValue::Dict(Dict::new_sync().insert(
            Key::atom_from_text("role"),
            CoreValue::Atom(crate::core::Atom::from_key(&Key::binary_from_text("macro"))),
        ));
        access.wrap(CoreValue::Dict(
            Dict::new_sync().insert(Key::atom_from_text("reasoning"), reasoning),
        ))
    });
    Diagnostic::from_parts(
        &public_values,
        diagnostic.source.clone(),
        diagnostic.severity,
        diagnostic.emission.clone(),
        Some(origin),
        diagnostic.line,
        diagnostic.message.clone(),
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

impl AssemblerReflectionHost {
    /// The environment this host was sealed with, a plain dictionary.
    fn sealed_environment(&self) -> &RuntimeValueRoot {
        self.reflection_environment
            .get()
            .expect("reasoning host must be sealed before it runs tasks")
    }
}

impl TaskEnvironment for AssemblerReflectionHost {
    /// The sealed environment, or, once machines supply reflection's
    /// environment, the `refl` machine's over it
    /// (`machines-inherit-an-environment`).
    fn reflection_environment(&self) -> Value {
        let sealed = self.sealed_environment();
        if self.resources.machine_environment() {
            Value::from_runtime_root(committed_environment(
                &self.resources,
                std::slice::from_ref(&*REFL),
                sealed,
            ))
        } else {
            sealed.value(self.resources.id)
        }
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
        let updated = {
            let mut state = self
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            // The client's conflict analysis runs here, under the transaction
            // lock, which is runtime core. A panic in it skips this status
            // publication rather than tearing the transaction state.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state.reflection.update_query(handle, result)
            }))
        };
        let Ok(updated) = updated else {
            return Box::new(|| {});
        };
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

    fn validate(
        &self,
        validation: crate::reflection::TaskValidation<'_, ReflectionEffects>,
    ) -> crate::reflection::ValidationResult {
        let (result, generation) = self.resources.validate_reflection(validation.store());
        match result {
            crate::reflection::StoreCommitResult::Committed => {
                crate::reflection::ValidationResult::Current { generation }
            }
            crate::reflection::StoreCommitResult::Conflict => {
                crate::reflection::ValidationResult::Conflict
            }
            crate::reflection::StoreCommitResult::MissingVolume(volume) => {
                crate::reflection::ValidationResult::MissingVolume(volume)
            }
        }
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
    /// The macro session every compile shares, made by the first: a
    /// compile ends when its front end does, and work its macros started
    /// runs on in the background, so no compile drains or revives one.
    compilation: Arc<Mutex<Option<Arc<CompilationExecution>>>>,
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
            compilation: Arc::default(),
        })
    }

    /// The macro session every compile shares.
    fn compilation_execution(&self) -> Result<Arc<CompilationExecution>, Error> {
        let mut compilation = self
            .compilation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(execution) = compilation.as_ref() {
            return Ok(execution.clone());
        }
        let execution = Arc::new(CompilationExecution::new(self)?);
        *compilation = Some(execution.clone());
        Ok(execution)
    }

    fn environment(&self) -> Value {
        self.host.reflection_environment()
    }

    /// The environment the session's host was sealed with, from which the
    /// environments of other roles derive.
    fn sealed_environment(&self) -> Value {
        self.host.sealed_environment().value(self.runtime.id())
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

/// The front-end compiler a source's extension selects. An extension with no
/// front end is rejected before its source loads, so an extension always
/// means what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrontEnd {
    /// The built-in `.g` compiler.
    G,
}

impl FrontEnd {
    fn for_extension(extension: &str) -> Result<Self, String> {
        match extension {
            "g" => Ok(Self::G),
            _ => Err(format!(
                "no front-end compiler for extension `.{extension}`; the built-in front end compiles `.g`"
            )),
        }
    }

    fn for_path(path: &Path) -> Result<Self, String> {
        let extension = path.extension().ok_or_else(|| {
            format!(
                "`{}` has no extension to select a front-end compiler",
                path.display()
            )
        })?;
        let extension = extension.to_str().ok_or_else(|| {
            format!(
                "`{}` has a non-UTF-8 extension, which selects no front-end compiler",
                path.display()
            )
        })?;
        Self::for_extension(extension)
    }

    fn compile(self, source: &[u8], context: &CompileContext) -> RuntimeValueRoot {
        match self {
            Self::G => compile_source(source, context),
        }
    }
}

/// An import site's provenance, for its compile's trace: its importer's
/// trace, and the request and namespace the site wrote; and its importer's
/// source cache, which a compile forced while the importer runs shares.
#[derive(Clone)]
pub(crate) struct ServedSite {
    parent: Arc<CompilationTrace>,
    import: Arc<ImportSite>,
    sources: Weak<SourceCache>,
}

/// A served source's label: its location's, or a script's.
fn record_label(record: &SourceRecord) -> String {
    match &record.scope.location {
        Some(location) => location.label(),
        None => format!("<script.{}>", record.lang),
    }
}

struct PreparedSource {
    front_end: FrontEnd,
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
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
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
    values.with_access(|access| {
        let CoreValue::Dict(root) = access.clone_core(&environment)? else {
            return Err(Error::new("reflection environment must be a dictionary"));
        };
        let glam_key = Key::atom_from_text("glam");
        let replaced_glam = root.get(&glam_key).is_some();
        Ok((
            access.wrap(CoreValue::Dict(root.insert(
                glam_key,
                authoritative_glam_environment(access.runtime_access(), role),
            ))),
            replaced_glam,
        ))
    })
}

fn authoritative_glam_environment(
    _access: &crate::core::RuntimeValueAccess<'_>,
    role: &str,
) -> CoreValue {
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
    values.with_access(|access| {
        let CoreValue::Dict(root) = access
            .clone_core(environment)
            .expect("authoritative reflection environment belongs to its runtime")
        else {
            unreachable!("authoritative reflection environment must be a dictionary")
        };
        access.wrap(CoreValue::Dict(root.insert(
            Key::atom_from_text("glam"),
            authoritative_glam_environment(access.runtime_access(), role),
        )))
    })
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
    source_system: Option<Arc<dyn SourceSystem>>,
    next_compilation_invocation: Arc<AtomicU64>,
    pub(super) reasoning: Arc<ReasoningSession>,
    diagnostic_attachments: Vec<DiagnosticAttachment>,
}

/// Non-owning route used by callbacks stored beneath the runtime's managed
/// graph. Source-system and scalar-ID state are safe to retain directly; the
/// reasoning session is weak so a deferred callback cannot retain its own
/// value domain through the external-owner registry.
#[derive(Clone)]
struct DeferredAssembler {
    source_system: Option<Arc<dyn SourceSystem>>,
    next_compilation_invocation: Arc<AtomicU64>,
    reasoning: Weak<ReasoningSession>,
}

impl DeferredAssembler {
    fn upgrade(&self) -> Option<Assembler> {
        Some(Assembler {
            source_system: self.source_system.clone(),
            next_compilation_invocation: self.next_compilation_invocation.clone(),
            reasoning: self.reasoning.upgrade()?,
            diagnostic_attachments: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct DiagnosticAttachment {
    _subscription: DiagnosticSubscription,
}

/// Staged construction of one assembler and its single reasoning session.
pub struct AssemblerBuilder {
    source_system: Option<Arc<dyn SourceSystem>>,
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
        values.with_access(|scoped| {
            let label = label.into();
            let promise = scoped
                .runtime_access()
                .construct_rooted_managed_promise()
                .expect("managed promise representation must fit one collector run");
            let value = scoped.wrap(CoreValue::Promised(PromisedValue::from_root(
                &promise,
                scoped.runtime_access(),
            )));
            (
                value,
                PromiseResolver {
                    observer: scoped.runtime_access().values().runtime_value_observer(),
                    label,
                    promise: Some(promise),
                },
            )
        })
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
            source_system: Some(Arc::new(FileSourceSystem::default())),
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
        self.source_system = Some(Arc::new(source_system));
        self
    }

    /// No source system: the host serves sources, and imports ask for them
    /// on the root's `imports` port (`imports-are-source-requests`).
    pub(crate) fn served_sources(mut self) -> Self {
        self.source_system = None;
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
            reasoning: Arc::new(reasoning),
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

    fn deferred(&self) -> DeferredAssembler {
        DeferredAssembler {
            source_system: self.source_system.clone(),
            next_compilation_invocation: self.next_compilation_invocation.clone(),
            reasoning: Arc::downgrade(&self.reasoning),
        }
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
            CompilationExecution::new(&self.reasoning)
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
        let values = self.values();
        values.with_access(|scoped| {
            let label = label.into();
            let promise = scoped
                .runtime_access()
                .construct_rooted_managed_promise()
                .expect("managed promise representation must fit one collector run");
            let value = scoped.wrap(CoreValue::Promised(PromisedValue::from_root(
                &promise,
                scoped.runtime_access(),
            )));
            (
                value,
                PromiseResolver {
                    observer: scoped.runtime_access().values().runtime_value_observer(),
                    label,
                    promise: Some(promise),
                },
            )
        })
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
        Value::from_runtime_root(crate::g_syntax::default_diagnostic_formatter_root(
            &self.core_values(),
        ))
    }

    /// Returns the read-only environment shared by reflection tasks in this
    /// assembler's evaluation session.
    pub fn reflection_environment(&self) -> Value {
        self.reasoning.environment()
    }

    /// Returns this session environment with another authoritative reasoning
    /// role. Service sessions retain the client-provided environment while
    /// identifying themselves independently from the assembler session. It
    /// derives from the environment the session was sealed with, beneath any
    /// that machines supply.
    pub fn reflection_environment_for_role(&self, role: impl AsRef<str>) -> Value {
        reflection_environment_for_role(
            &self.values(),
            &self.reasoning.sealed_environment(),
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
        self.publish_unrecognized_annotation_warnings();
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

    /// Publishes a warning for each annotation evaluation recorded as
    /// unrecognized since the last drain. Evaluation itself performs no I/O.
    pub(super) fn publish_unrecognized_annotation_warnings(&self) {
        let pending = self.core_values().take_unrecognized_annotations();
        if pending.is_empty() {
            return;
        }
        let values = self.values();
        let bus = self.diagnostic_bus();
        for rendered in pending {
            bus.publish_local(Diagnostic::new(
                &values,
                Severity::Warning,
                format!("unrecognized annotation: {rendered}"),
            ));
        }
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
        Ok(values.with_access(|values| {
            let runtime = values
                .runtime_access()
                .construct_managed_core_net(template.instantiate_with(values.runtime_access()))
                .expect("managed core-net representation must fit one collector run");
            values.wrap(CoreValue::Net(NetValue::new(runtime)))
        }))
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
        let execution = self.reasoning.compilation_execution()?;
        let result = self.build_module_inner(
            module_path,
            inputs,
            initial_definitions.into_runtime_root(),
            session.clone(),
            execution,
        );
        let diagnostics = session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match result {
            Ok(value) => Ok(BuiltModule {
                value: Value::from_runtime_root(value),
                diagnostics,
            }),
            Err(error) => Err(error.with_diagnostics(diagnostics)),
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
        let module_loader = self.module_loader(session.clone());
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
            definitions = prepared
                .front_end
                .compile(prepared.source.bytes(), &prepared.context);
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
        // What the source's imports are relative to: a script has no place,
        // so it imports nothing.
        let (front_end, source, importer) = match input {
            ModuleInput::File(path) => {
                let front_end = FrontEnd::for_path(path).map_err(Error::new)?;
                let source_system = self.source_system.as_ref().ok_or_else(|| {
                    Error::new(format!(
                        "`{}` is a file, but this runtime's host serves its sources: it hands over a top-level source as `ModuleInput::source`",
                        path.display()
                    ))
                })?;
                let source = source_system
                    .load_top_level(path)
                    .map_err(|error| Error::new(error.to_string()))?;
                (front_end, Arc::new(source), true)
            }
            ModuleInput::Script { extension, body } => {
                let front_end = FrontEnd::for_extension(extension).map_err(Error::new)?;
                let label: Arc<str> = Arc::from(format!("<script.{extension}>"));
                let source =
                    SourceArtifact::new(body.clone(), SourceIdentity::script(label, body.clone()));
                (front_end, Arc::new(source), false)
            }
        };
        let CompileSetup {
            module_path,
            prior_defs,
            final_defs,
            module_loader,
            binary_loader,
            session,
            execution,
        } = setup;
        let trace = Arc::new(CompilationTrace::root(
            self.next_compilation_invocation(),
            &source,
            module_path.clone(),
        ));
        let had_errors = Arc::new(AtomicBool::new(false));
        let mut context = CompileContext::from_module_path_with_values(
            self.core_values(),
            module_path.iter().cloned(),
        );
        if importer {
            context = context.with_importer_source(source.clone());
        }
        let context = context
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
            front_end,
            source,
            context,
            had_errors,
        })
    }

    fn module_loader(&self, session: Arc<Mutex<Vec<Diagnostic>>>) -> ModuleLoader {
        let assembler = self.deferred();
        Arc::new(move |args| {
            let failure = |values: Option<&CoreValueFactory>, message: String| {
                import_failure(
                    values,
                    message,
                    args.request.as_str(),
                    args.importer_trace.as_deref(),
                    args.importer_source.as_deref(),
                )
            };
            let Some(assembler) = assembler.upgrade() else {
                return Err(failure(
                    None,
                    format!(
                        "local import `{}` cannot run after its assembler was dropped",
                        args.request.as_str()
                    ),
                ));
            };
            let values = assembler.core_values();
            let failure = |message: String| {
                failure(
                    Some(&values),
                    format!("local import `{}` {message}", args.request.as_str()),
                )
            };
            assembler.compile_in_execution(&failure, |execution| {
                assembler.load_local_module(args.clone(), session.clone(), execution)
            })
        })
    }

    /// Runs `compile` in the macro session every compile shares; it ends
    /// when its front end does.
    fn compile_in_execution(
        &self,
        failure: &dyn Fn(String) -> Arc<EvaluationFailure>,
        compile: impl FnOnce(
            Arc<CompilationExecution>,
        ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>>,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let execution = self.reasoning.compilation_execution().map_err(|error| {
            failure(format!(
                "could not create its compilation execution: {error}"
            ))
        })?;
        compile(execution)
    }

    fn binary_loader(&self) -> BinaryFileLoader {
        let assembler = self.deferred();
        Arc::new(move |args| {
            let Some(assembler) = assembler.upgrade() else {
                return Err(import_failure(
                    None,
                    format!(
                        "binary import `{}` cannot run after its assembler was dropped",
                        args.request.as_str()
                    ),
                    args.request.as_str(),
                    args.importer_trace.as_deref(),
                    args.importer_source.as_deref(),
                ));
            };
            assembler.load_local_binary(args)
        })
    }

    /// The compiler for a client's compile, `site` absent, or for one
    /// import site (`compile-yields-an-open-mixin`).
    pub(crate) fn source_compiler(&self, site: Option<ServedSite>) -> SourceCompiler {
        let assembler = self.deferred();
        SourceCompiler(Arc::new(move |record, call| {
            let subject = match &site {
                Some(site) => format!("local import `{}`", site.import.request),
                None => format!("source `{}`", record_label(&record)),
            };
            let Some(assembler) = assembler.upgrade() else {
                return Err(Arc::new(EvaluationFailure::message(format!(
                    "{subject} cannot compile after its assembler was dropped"
                ))));
            };
            let failure = |message: String| {
                Arc::new(EvaluationFailure::message(format!("{subject} {message}")))
            };
            assembler.compile_in_execution(&failure, |execution| {
                assembler.compile_served(record, call, site.clone(), execution)
            })
        }))
    }

    /// Writes a request for the source a key names
    /// (`imports-are-source-requests`). The route holds the runtime weakly,
    /// since import values beneath its managed graph keep it.
    pub(crate) fn source_requester(&self) -> SourceRequester {
        let resources = Arc::downgrade(&self.evaluation_runtime().shared_resources());
        let values = self.core_values();
        Arc::new(move |key| {
            let resources = resources.upgrade().ok_or_else(|| {
                Arc::new(EvaluationFailure::message(
                    "a source cannot be asked for after its runtime was dropped",
                ))
            })?;
            let promises = resources.reflection_snapshot().1.promises().clone();
            let (reply, request) = values.with_boundary_access(|access| {
                let (_, reply, resolver) = promises.allocate(&access);
                let request = Dict::new_sync()
                    .insert(keys::REQUEST.clone(), key.clone_core_with(&access))
                    .insert(keys::REPLY.clone(), resolver);
                (
                    access.root_runtime_value(reply),
                    access.root_runtime_value(CoreValue::Dict(request)),
                )
            });
            admit_runtime_request(&resources, request);
            Ok(reply)
        })
    }

    /// Compiles a served record's source for one application, on the thread
    /// that forces it. Where a compile-time evaluation waited on a reply, the
    /// attempt yields what waits for it and compiles again, with what the
    /// attempt learned (`compile-yields-an-open-mixin`); its diagnostics go
    /// with it.
    fn compile_served(
        &self,
        record: SourceRecord,
        call: CompileCall,
        site: Option<ServedSite>,
        execution: Arc<CompilationExecution>,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let values = self.core_values();
        let label = record_label(&record);
        let failure = |message: String, trace: Option<&CompilationTrace>, source| match &site {
            Some(site) => import_failure(
                Some(&values),
                message,
                &site.import.request,
                trace.or(Some(&site.parent)),
                source,
            ),
            None => Arc::new(EvaluationFailure::message(message)),
        };
        let front_end = FrontEnd::for_extension(&record.lang)
            .map_err(|message| failure(format!("`{label}`: {message}"), None, None))?;
        let identity = match &record.scope.location {
            Some(location) => SourceIdentity::keyed(label.clone(), location.key()),
            None => SourceIdentity::script(label.clone(), record.source.clone()),
        };
        let source = Arc::new(SourceArtifact::new(record.source.clone(), identity));
        let namespace = record.scope.namespace.clone();
        let (invocation, replayed) = self.replayed(&call.replay);
        let invocation = invocation.unwrap_or_else(|| self.next_compilation_invocation());
        // A compile forced while its importer runs shares the importer's
        // cache, so that the importer, compiling again, asks for nothing
        // this compile asked for.
        let sources = match site.as_ref().and_then(|site| site.sources.upgrade()) {
            Some(shared) => {
                let mut cached = shared.lock().unwrap_or_else(PoisonError::into_inner);
                for (key, source) in replayed {
                    if !cached.iter().any(|(asked, _)| *asked == key) {
                        cached.push((key, source));
                    }
                }
                drop(cached);
                shared
            }
            None => Arc::new(Mutex::new(replayed)),
        };
        let trace = Arc::new(match &site {
            Some(site) => CompilationTrace::imported(
                invocation,
                &source,
                namespace.clone(),
                site.parent.clone(),
                site.import.request.clone(),
                site.import.extends.clone(),
            ),
            None => CompilationTrace::root(invocation, &source, namespace.clone()),
        });
        let had_errors = Arc::new(AtomicBool::new(false));
        let held = Arc::new(Mutex::new(Vec::new()));
        let compilers = self.import_compilers(trace.clone(), Arc::downgrade(&sources));
        let context =
            CompileContext::from_module_path_with_values(values.clone(), namespace.iter().cloned())
                .with_importer_source(source.clone())
                .with_compilation_trace(trace.clone())
                .with_prior_defs_root(call.base.clone())
                .with_final_defs_root(call.self_value.clone())
                .with_served_imports(ServedImports {
                    location: record.scope.location.clone(),
                    compilers,
                    requester: self.source_requester(),
                    sources: sources.clone(),
                })
                .with_compilation_execution(execution)
                .with_diagnostic_emitter(self.served_diagnostic_emitter(
                    trace.clone(),
                    had_errors.clone(),
                    held.clone(),
                ));
        let definitions = front_end.compile(source.bytes(), &context);
        if let Some(reply) = context.take_wait() {
            let sources = sources
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            return Ok(self.compile_again(call, reply, invocation, sources));
        }
        for diagnostic in std::mem::take(&mut *held.lock().unwrap_or_else(PoisonError::into_inner))
        {
            self.record_diagnostic(diagnostic);
        }
        if had_errors.load(Ordering::Relaxed) {
            return Err(failure(
                format!("`{label}` failed to compile"),
                Some(&trace),
                Some(&source),
            ));
        }
        Ok(definitions)
    }

    /// What earlier attempts of a compile learned, from its `replay`
    /// argument, `{invocation, sources}`: its invocation, and the sources it
    /// asked for, by key. The runtime builds that argument, so it is data
    /// whose outer dicts are evaluated.
    fn replayed(
        &self,
        replay: &RuntimeValueRoot,
    ) -> (
        Option<CompilationInvocationId>,
        Vec<(Key, RuntimeValueRoot)>,
    ) {
        self.core_values().with_boundary_access(|access| {
            replay
                .with_core(&access, |replay| {
                    let CoreValue::Dict(replay) = replay else {
                        return (None, Vec::new());
                    };
                    let invocation = match replay.get(&keys::INVOCATION) {
                        Some(CoreValue::Number(number)) => number
                            .to_u64_if_integer()
                            .filter(|id| *id != 0)
                            .map(CompilationInvocationId::new),
                        _ => None,
                    };
                    let sources = match replay.get(&keys::SOURCES) {
                        Some(CoreValue::Dict(sources)) => sources
                            .iter()
                            .map(|(key, source)| {
                                (
                                    key.clone(),
                                    access.root_runtime_value(access.duplicate_value(source)),
                                )
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    (invocation, sources)
                })
                .expect("a compile's replay belongs to its runtime")
        })
    }

    /// `seq reply (compile_source compiler record replay base self)`: waits
    /// for the reply, then compiles again, with what this attempt learned.
    fn compile_again(
        &self,
        call: CompileCall,
        reply: ManagedPromiseRoot,
        invocation: CompilationInvocationId,
        sources: Vec<(Key, RuntimeValueRoot)>,
    ) -> RuntimeValueRoot {
        self.core_values().construct_runtime_value_root(|access| {
            let sources = sources
                .iter()
                .fold(Dict::new_sync(), |dict, (key, source)| {
                    dict.insert(key.clone(), source.clone_core_with(access))
                });
            let replay = Dict::new_sync()
                .insert(
                    keys::INVOCATION.clone(),
                    CoreValue::Number(Number::from_u64(invocation.get())),
                )
                .insert(keys::SOURCES.clone(), CoreValue::Dict(sources));
            let again = CoreValue::Lazy(LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin: Builtin::CompileSource,
                    arguments: Arc::from([
                        call.compiler.clone_core_with(access),
                        call.record.clone_core_with(access),
                        CoreValue::Dict(replay),
                        call.base.clone_core_with(access),
                        call.self_value.clone_core_with(access),
                    ]),
                },
            ));
            let reply = CoreValue::Promised(PromisedValue::from_root(&reply, access));
            CoreValue::builtin_call_in(access, Builtin::Seq, vec![reply, again])
        })
    }

    /// A compiler for each import site of the compilation `trace` records.
    fn import_compilers(
        &self,
        trace: Arc<CompilationTrace>,
        sources: Weak<SourceCache>,
    ) -> ImportCompilers {
        let assembler = self.deferred();
        Arc::new(move |import| {
            let site = ServedSite {
                parent: trace.clone(),
                import: Arc::new(import),
                sources: sources.clone(),
            };
            match assembler.upgrade() {
                Some(assembler) => assembler.source_compiler(Some(site)),
                None => SourceCompiler(Arc::new(|_, _| {
                    Err(Arc::new(EvaluationFailure::message(
                        "an import cannot compile after its assembler was dropped",
                    )))
                })),
            }
        })
    }

    fn load_local_module(
        &self,
        args: ModuleLoadArgs,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        execution: Arc<CompilationExecution>,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let importer = args.importer_source.as_ref().ok_or_else(|| {
            import_failure(
                Some(&self.core_values()),
                format!(
                    "local import `{}` cannot be loaded from a source without an import resolver",
                    args.request.as_str()
                ),
                args.request.as_str(),
                args.importer_trace.as_deref(),
                None,
            )
        })?;
        let front_end =
            FrontEnd::for_path(Path::new(args.request.as_str())).map_err(|message| {
                import_failure(
                    Some(&self.core_values()),
                    format!("local import `{}`: {message}", args.request.as_str()),
                    args.request.as_str(),
                    args.importer_trace.as_deref(),
                    Some(importer),
                )
            })?;
        let source = Arc::new(importer.load_relative(&args.request).map_err(|error| {
            import_failure(
                Some(&self.core_values()),
                format!(
                    "local import `{}` could not be loaded: {error}",
                    args.request.as_str()
                ),
                args.request.as_str(),
                args.importer_trace.as_deref(),
                Some(importer),
            )
        })?);
        self.compile_local_module(front_end, args, source, session, execution)
    }

    /// Compiles an imported module's source, loaded or served.
    fn compile_local_module(
        &self,
        front_end: FrontEnd,
        args: ModuleLoadArgs,
        source: Arc<SourceArtifact>,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        execution: Arc<CompilationExecution>,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let module_loader = self.module_loader(session.clone());
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
        let definitions = front_end.compile(source.bytes(), &context);

        if had_errors.load(Ordering::Relaxed) {
            Err(import_failure(
                Some(&self.core_values()),
                format!(
                    "local import `{}` failed to compile",
                    source.identity().label()
                ),
                args.request.as_str(),
                Some(&trace),
                Some(&source),
            ))
        } else {
            Ok(definitions)
        }
    }

    fn load_local_binary(
        &self,
        args: BinaryLoadArgs,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let importer = args.importer_source.as_ref().ok_or_else(|| {
            import_failure(
                Some(&self.core_values()),
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
                self.core_values()
                    .construct_runtime_value_root(|_| CoreValue::Binary(artifact.bytes().clone()))
            })
            .map_err(|error| {
                import_failure(
                    Some(&self.core_values()),
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
        let published = self.core_values().with_boundary_access(|access| {
            let CoreValue::Promised(final_defs) = context.final_defs(&access) else {
                panic!("CompileContext.final_defs must be a promised value");
            };
            let final_defs = final_defs.root_in(&access);
            final_defs.publish(&access, Ok(definitions.clone_core_with(&access)))
        });
        let published = match published {
            Ok(published) => published,
            Err(_) => panic!("CompileContext.final_defs future must be unassigned"),
        };
        published.notify();
        self.eval_context()
            .evaluate_root_whnf(definitions.clone())
            .map_err(|error| self.evaluation_error(error))
    }

    /// Holds a served compile's diagnostics for its attempt, which publishes
    /// them where the runtime's own go once it completes, or drops them
    /// where it waits and compiles again (`compile-yields-an-open-mixin`).
    fn served_diagnostic_emitter(
        &self,
        trace: Arc<CompilationTrace>,
        had_errors: Arc<AtomicBool>,
        held: Arc<Mutex<Vec<Diagnostic>>>,
    ) -> CompileDiagnosticEmitter {
        let values = self.values();
        Arc::new(move |severity, message| {
            if severity == Severity::Error {
                had_errors.store(true, Ordering::Relaxed);
            }
            let diagnostic = Diagnostic::from_compile(values.core(), &trace, severity, &message);
            held.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(diagnostic);
        })
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
                Diagnostic::from_compile(assembler.values().core(), &trace, severity, &message);
            session
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_compiler_loaders_do_not_retain_their_runtime() {
        let assembler = Assembler::new();
        let observer = assembler.core_values().runtime_value_observer();
        let session = Arc::new(Mutex::new(Vec::new()));
        let module_loader = assembler.module_loader(session);
        let binary_loader = assembler.binary_loader();

        drop(assembler);
        assert!(
            observer.upgrade().is_none(),
            "deferred compiler callbacks must keep only weak routes into their runtime"
        );

        drop((module_loader, binary_loader));
    }
}
