use std::sync::{Arc, Condvar, Mutex, Weak};

use super::machine::{ContextualValueEffectTask, EffectTask, UnitEffectTask, ValueEffectTask};
use super::protocol::{StandardEffects, TaskHalt, TaskHost, TaskOutcome, TaskSpecialization};
use crate::api::{DiagnosticIngress, EvaluationRuntime, Value as PublicValue};
use crate::core::{EvaluationFailure, Value};
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationSession, EvaluationSessionRun, EvaluationTaskHandle,
    EvaluationTaskMachine, EvaluationTaskStatus, EvaluationWaitPoll, PreparedEvaluationTask,
    ReflectionTaskLauncher, ReflectionTaskProfile, ReflectionTaskResultPolicy, TaskStatusPublisher,
    TaskStatusWake,
};
use crate::runtime::RuntimeFailureRoot;

/// Host-owned observation of one coordinator-managed composed effect root.
///
/// The coordinator retains only a weak publication route. Dropping this
/// handle therefore disables lifecycle observation without retaining the
/// task, its demand session, or its evaluation runtime.
#[doc(hidden)]
#[derive(Clone)]
pub struct EffectLifecycle {
    inner: Arc<EffectLifecycleState>,
    terminal: Option<EffectLifecycleTerminal>,
}

struct EffectLifecycleState {
    status: Mutex<EffectLifecycleStatus>,
    changed: Condvar,
}

/// The last committed scheduler status published for a composed effect root.
#[doc(hidden)]
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum EffectLifecycleStatus {
    Launched,
    Blocked,
    Complete(PublicValue),
    Failed(TaskHalt),
    Cancelled,
    Abandoned,
    Exited,
    Killed(TaskHalt),
}

/// Opaque coordinator-terminal policy for one host-owned effect lifecycle.
///
/// Construction is supplied by runtime facilities such as a diagnostic
/// ingress. The guarded transition remains internal; embedding clients can
/// attach the resulting policy without gaining runtime mutation authority.
#[doc(hidden)]
#[derive(Clone)]
pub struct EffectLifecycleTerminal {
    runtime: crate::runtime::EvaluationRuntimeId,
    publisher: TaskStatusPublisher,
}

impl EffectLifecycleTerminal {
    pub(crate) fn new(
        runtime: crate::runtime::EvaluationRuntimeId,
        publisher: TaskStatusPublisher,
    ) -> Self {
        Self { runtime, publisher }
    }

    fn publish_guarded(
        &self,
        mutation: &dyn crate::runtime::RuntimeMutationAuthority,
        status: EvaluationTaskStatus,
    ) -> TaskStatusWake {
        self.publisher.publish_guarded(mutation, status)
    }
}

impl EffectLifecycleStatus {
    fn same_state(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Launched | Self::Blocked)
    }
}

impl EffectLifecycle {
    pub fn new(_runtime: &EvaluationRuntime) -> Self {
        Self {
            inner: Arc::new(EffectLifecycleState {
                status: Mutex::new(EffectLifecycleStatus::Launched),
                changed: Condvar::new(),
            }),
            terminal: None,
        }
    }

    /// Constructs a lifecycle whose terminal publication also performs one
    /// host-selected guarded transition. This is reserved for
    /// coordinator-owned roots; direct synchronous effects have no such
    /// terminal dispatch.
    #[doc(hidden)]
    pub fn new_with_terminal(
        runtime: &EvaluationRuntime,
        terminal: EffectLifecycleTerminal,
    ) -> Self {
        assert_eq!(
            runtime.id(),
            terminal.runtime,
            "effect lifecycle terminal policy belongs to another evaluation runtime"
        );
        Self {
            inner: Arc::new(EffectLifecycleState {
                status: Mutex::new(EffectLifecycleStatus::Launched),
                changed: Condvar::new(),
            }),
            terminal: Some(terminal),
        }
    }

    pub fn status(&self) -> EffectLifecycleStatus {
        self.inner
            .status
            .lock()
            .expect("effect lifecycle mutex should not be poisoned")
            .clone()
    }

    pub fn wait_for_terminal(&self) -> EffectLifecycleStatus {
        let mut status = self
            .inner
            .status
            .lock()
            .expect("effect lifecycle mutex should not be poisoned");
        while !status.is_terminal() {
            status = self
                .inner
                .changed
                .wait(status)
                .expect("effect lifecycle mutex should not be poisoned");
        }
        status.clone()
    }

    pub fn wait_for_change(&self, observed: &EffectLifecycleStatus) -> EffectLifecycleStatus {
        let mut status = self
            .inner
            .status
            .lock()
            .expect("effect lifecycle mutex should not be poisoned");
        while status.same_state(observed) {
            status = self
                .inner
                .changed
                .wait(status)
                .expect("effect lifecycle mutex should not be poisoned");
        }
        status.clone()
    }

    fn publisher(
        &self,
        session: Arc<Mutex<Option<Arc<EvaluationSession>>>>,
    ) -> TaskStatusPublisher {
        let lifecycle = Arc::downgrade(&self.inner);
        let terminal = self.terminal.clone();
        TaskStatusPublisher::new(move |mutation, status| {
            let Some(lifecycle) = Weak::upgrade(&lifecycle) else {
                return TaskStatusWake::new(|| {});
            };
            let terminal_status = status.clone();
            let status = lifecycle.public_status(status);
            let is_terminal = status.is_terminal();
            let terminal_wake = if is_terminal {
                terminal
                    .as_ref()
                    .map(|terminal| terminal.publish_guarded(mutation, terminal_status))
            } else {
                None
            };
            *lifecycle
                .status
                .lock()
                .expect("effect lifecycle mutex should not be poisoned") = status;
            let session = session.clone();
            TaskStatusWake::new(move || {
                lifecycle.changed.notify_all();
                if is_terminal && terminal_wake.is_some() {
                    let session = session
                        .lock()
                        .expect("scheduled effect session mutex should not be poisoned")
                        .take();
                    drop(session);
                }
                if let Some(wake) = terminal_wake {
                    wake.notify();
                }
            })
        })
    }
}

impl EffectLifecycleState {
    fn public_status(&self, status: EvaluationTaskStatus) -> EffectLifecycleStatus {
        match status {
            EvaluationTaskStatus::Launched => EffectLifecycleStatus::Launched,
            EvaluationTaskStatus::Blocked => EffectLifecycleStatus::Blocked,
            EvaluationTaskStatus::Complete(value) => {
                EffectLifecycleStatus::Complete(PublicValue::from_runtime_root(value))
            }
            EvaluationTaskStatus::Failed(error) => {
                EffectLifecycleStatus::Failed(TaskHalt::rooted_failure(error))
            }
            EvaluationTaskStatus::Cancelled => EffectLifecycleStatus::Cancelled,
            EvaluationTaskStatus::Abandoned => EffectLifecycleStatus::Abandoned,
            EvaluationTaskStatus::Exited => EffectLifecycleStatus::Exited,
            EvaluationTaskStatus::Killed(error) => {
                EffectLifecycleStatus::Killed(TaskHalt::rooted_failure(error))
            }
        }
    }
}

/// A composed effect root retained by the runtime coordinator rather than a
/// caller's Rust stack.
#[doc(hidden)]
pub struct ScheduledEffectRun {
    context: EvalContext,
    session: Arc<Mutex<Option<Arc<EvaluationSession>>>>,
    task: EvaluationTaskHandle,
}

impl Drop for ScheduledEffectRun {
    fn drop(&mut self) {
        let session = self
            .session
            .lock()
            .expect("scheduled effect session mutex should not be poisoned")
            .take();
        drop(session);
    }
}

impl ScheduledEffectRun {
    pub fn run(self) -> Result<TaskOutcome, TaskHalt> {
        loop {
            let children = self.context.run_until_quiescent();
            match self.context.poll_reflection_task(&self.task) {
                EvaluationWaitPoll::Pending(_) => {
                    if self.context.has_ready_session_task() {
                        continue;
                    }
                    self.context.wait_for_task_change(&self.task);
                }
                EvaluationWaitPoll::Complete(value) => {
                    return combine_composed_result(
                        Ok(TaskOutcome::Complete(PublicValue::from_runtime_root(
                            *value,
                        ))),
                        children,
                    );
                }
                EvaluationWaitPoll::Failed(error) => {
                    return combine_composed_result(Err(TaskHalt::rooted_failure(error)), children);
                }
                EvaluationWaitPoll::Cancelled => {
                    return combine_composed_result(Ok(TaskOutcome::Cancelled), children);
                }
                EvaluationWaitPoll::Abandoned => {
                    return combine_composed_result(
                        Err(TaskHalt::new("scheduled effect root was abandoned")),
                        children,
                    );
                }
                EvaluationWaitPoll::Exited => {
                    return combine_composed_result(
                        Err(TaskHalt::new(
                            "scheduled effect root exited without a result",
                        )),
                        children,
                    );
                }
                EvaluationWaitPoll::Killed(error) => {
                    return combine_composed_result(Err(TaskHalt::rooted_failure(error)), children);
                }
            }
        }
    }

    pub fn cancel(&self) {
        let _ = self.task.cancel();
    }
}

/// Configures and synchronously runs one effect task.
///
/// `.task.new` children inherit this task's complete specialization and host
/// profile. All scheduled children are drained before the run returns; a
/// child failure or stable deadlock fails the composed run.
pub struct EffectRun<S: TaskSpecialization> {
    effect: PublicValue,
    specialization: S,
    host: Arc<S::Host>,
    runtime: Option<EvaluationRuntime>,
    result_policy: EffectResultPolicy,
    result_assertion_context: Option<Arc<str>>,
    failure_context: Option<PublicValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectResultPolicy {
    Return,
    RequireUnit,
}

impl<S: TaskSpecialization> EffectRun<S> {
    pub fn new(
        runtime: &EvaluationRuntime,
        effect: &PublicValue,
        specialization: S,
        host: Arc<S::Host>,
    ) -> Self {
        Self {
            effect: effect.clone(),
            specialization,
            host,
            runtime: Some(runtime.clone()),
            result_policy: EffectResultPolicy::Return,
            result_assertion_context: None,
            failure_context: None,
        }
    }

    /// Requires the task endpoint to return unit.
    ///
    /// This is a provenance-free safety policy. Providers that know why the
    /// result is discarded should also install [`Self::asserting_unit_result`]
    /// with their own diagnostic context.
    pub fn requiring_unit_result(mut self) -> Self {
        self.result_policy = EffectResultPolicy::RequireUnit;
        self
    }

    /// Checks the result through the ordinary parameterized unit assertion
    /// before applying the task endpoint's generic result policy.
    pub fn asserting_unit_result(mut self, diagnostic_context: impl Into<Arc<str>>) -> Self {
        self.result_assertion_context = Some(diagnostic_context.into());
        self
    }

    /// Prepends one semantic context frame to a permanent failure from the
    /// complete effect run, including failures reached after effect dispatch.
    pub fn contextualizing_failures(mut self, context: PublicValue) -> Result<Self, TaskHalt> {
        let runtime = self
            .runtime
            .as_ref()
            .expect("EffectRun construction always selects an evaluation runtime");
        if context.runtime_id() != runtime.id() {
            return Err(TaskHalt::new(format!(
                "effect failure context belongs to evaluation runtime {}, not {}",
                context.runtime_id().get(),
                runtime.id().get()
            )));
        }
        self.failure_context = Some(context);
        Ok(self)
    }

    pub fn run(self) -> Result<TaskOutcome, TaskHalt> {
        let Self {
            effect,
            specialization,
            host,
            runtime,
            result_policy,
            result_assertion_context,
            failure_context,
        } = self;
        let task_profile = Arc::new(ReflectionTaskProfile::sealed(task_launcher(
            specialization.clone(),
            host.clone(),
        )));
        let runtime = runtime.expect("EffectRun construction always selects an evaluation runtime");
        let runtime_id = runtime.id();
        let values = runtime.values();
        let session = runtime.new_evaluation_session()?;
        let mut task = EffectTask::new_in_context(
            effect.into_core(),
            specialization,
            host,
            EvalContext::with_task_profile(&session, task_profile),
        )
        .map_err(|error| {
            contextualize_task_halt(error, &values, failure_context.as_ref())
                .root_for_runtime(runtime_id)
        })?;
        if result_policy == EffectResultPolicy::RequireUnit {
            task = task.requiring_unit_result();
        }
        if let Some(diagnostic_context) = result_assertion_context {
            task = task.asserting_unit_result(diagnostic_context);
        }
        run_composed_effect_task(task).map_err(|error| {
            contextualize_task_halt(error, &values, failure_context.as_ref())
                .root_for_runtime(runtime_id)
        })
    }

    /// Installs this effect as coordinator work in a fresh demand session.
    /// The returned handle retains the hidden task and session lease; child
    /// `.task.new` operations inherit the same complete, exit-capable task
    /// profile.
    #[doc(hidden)]
    pub fn schedule(self, lifecycle: &EffectLifecycle) -> Result<ScheduledEffectRun, TaskHalt> {
        self.schedule_with_capabilities(lifecycle, None)
    }

    /// Installs this effect as the consumer of a runtime diagnostic ingress.
    /// Route activation and coordinator-root activation are settlement-atomic.
    #[doc(hidden)]
    pub fn schedule_diagnostic_consumer(
        self,
        lifecycle: &EffectLifecycle,
        ingress: &DiagnosticIngress,
    ) -> Result<ScheduledEffectRun, TaskHalt> {
        self.schedule_with_capabilities(lifecycle, Some(ingress))
    }

    fn schedule_with_capabilities(
        self,
        lifecycle: &EffectLifecycle,
        diagnostic_ingress: Option<&DiagnosticIngress>,
    ) -> Result<ScheduledEffectRun, TaskHalt> {
        let Self {
            effect,
            specialization,
            host,
            runtime,
            result_policy,
            result_assertion_context,
            failure_context,
        } = self;
        let task_profile = Arc::new(ReflectionTaskProfile::sealed(coordinator_task_launcher(
            specialization.clone(),
            host.clone(),
        )));
        let runtime = runtime.expect("EffectRun construction always selects an evaluation runtime");
        let session = runtime.new_evaluation_session_with_profile(task_profile)?;
        let context = EvalContext::new(&session);
        let session = Arc::new(Mutex::new(Some(session)));
        let failure_context = failure_context.map(PublicValue::into_core);
        let prepared = context
            .prepare_machine(
                Some(lifecycle.publisher(session.clone())),
                move |task_context| {
                    let mut task = EffectTask::new_in_context_with_capabilities(
                        effect.into_core(),
                        specialization,
                        host,
                        task_context,
                        false,
                        true,
                    )
                    .map_err(|error| {
                        let failure = error.into_failure();
                        match &failure_context {
                            Some(context) => Arc::new(failure.with_context(context.clone())),
                            None => failure,
                        }
                    })?;
                    if result_policy == EffectResultPolicy::RequireUnit {
                        task = task.requiring_unit_result();
                    }
                    if let Some(diagnostic_context) = result_assertion_context {
                        task = task.asserting_unit_result(diagnostic_context);
                    }
                    Ok(match failure_context {
                        Some(context) => Box::new(ContextualValueEffectTask::new(task, context))
                            as Box<dyn EvaluationTaskMachine>,
                        None => Box::new(ValueEffectTask(task)) as Box<dyn EvaluationTaskMachine>,
                    })
                },
            )
            .map_err(|failure| {
                TaskHalt::rooted_failure(RuntimeFailureRoot::from_runtime(runtime.id(), failure))
            })?;
        let task = match diagnostic_ingress {
            Some(ingress) => {
                runtime
                    .activate_diagnostic_consumer(ingress, |mutation| {
                        prepared.activate_guarded(mutation)
                    })
                    .map_err(|error| TaskHalt::new(error.to_string()))?;
                prepared.finish_guarded_activation(true);
                prepared.into_handle()
            }
            None => PreparedEvaluationTask::activate(prepared),
        };
        Ok(ScheduledEffectRun {
            context,
            session,
            task,
        })
    }
}

fn contextualize_task_halt(
    error: TaskHalt,
    values: &crate::api::Values,
    context: Option<&PublicValue>,
) -> TaskHalt {
    match context {
        Some(context) => error.with_context(values, context.clone()),
        None => error,
    }
}

/// Runs one reflection effect with a statically selected set of extra effects.
pub fn run<S: TaskSpecialization>(
    runtime: &EvaluationRuntime,
    effect: &PublicValue,
    specialization: S,
    host: Arc<S::Host>,
) -> Result<TaskOutcome, TaskHalt> {
    EffectRun::new(runtime, effect, specialization, host).run()
}

pub(super) fn run_composed_effect_task<S: TaskSpecialization>(
    mut task: EffectTask<S>,
) -> Result<TaskOutcome, TaskHalt> {
    let runtime = task.eval_context.values().runtime_id();
    let parent = task.run();
    let children = task.eval_context.run_until_quiescent();
    combine_composed_result(parent, children).map_err(|error| error.root_for_runtime(runtime))
}

fn combine_composed_result(
    parent: Result<TaskOutcome, TaskHalt>,
    children: EvaluationSessionRun,
) -> Result<TaskOutcome, TaskHalt> {
    let child_error = composed_child_error(children);
    match (parent, child_error) {
        (Ok(outcome), None) => Ok(outcome),
        (Ok(_), Some(error)) | (Err(error), None) => Err(error),
        (Err(parent), Some(children)) => {
            let child_failure = children
                .permanent_failure()
                .expect("composed child reporting produces a permanent failure");
            Err(parent.with_core_context(eval::failure_diagnostic_value(child_failure)))
        }
    }
}

fn composed_child_error(run: EvaluationSessionRun) -> Option<TaskHalt> {
    let (quiescent, report) = match run {
        EvaluationSessionRun::Complete(report) => (false, report),
        EvaluationSessionRun::Quiescent(report) | EvaluationSessionRun::Deadlocked(report) => {
            (true, report)
        }
    };
    if report.failures.is_empty() && !quiescent {
        return None;
    }

    let mut details = Vec::new();
    for (task, error) in report.failures.iter() {
        details.push(format!(
            "task {} failed: {}",
            task.get(),
            error.as_failure()
        ));
    }
    if quiescent {
        details.push(format!(
            "scheduler deadlocked with {} unfinished task{}",
            report.unfinished.len(),
            if report.unfinished.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    Some(TaskHalt::new(details.join("; ")))
}

/// Builds a type-erased launcher for annotation and joinable reflection tasks.
pub(crate) fn task_launcher<S: TaskSpecialization>(
    specialization: S,
    host: Arc<S::Host>,
) -> Arc<dyn ReflectionTaskLauncher> {
    effect_task_launcher(specialization, host, false)
}

/// Builds a launcher for reflection tasks owned by the runtime coordinator.
/// Such tasks participate in quiescence settlement and therefore expose the
/// divergent `.exit.*` coordination family.
pub(crate) fn coordinator_task_launcher<S: TaskSpecialization>(
    specialization: S,
    host: Arc<S::Host>,
) -> Arc<dyn ReflectionTaskLauncher> {
    effect_task_launcher(specialization, host, true)
}

fn effect_task_launcher<S: TaskSpecialization>(
    specialization: S,
    host: Arc<S::Host>,
    exposes_exit: bool,
) -> Arc<dyn ReflectionTaskLauncher> {
    Arc::new(EffectTaskLauncher {
        specialization,
        host,
        exposes_exit,
    })
}

struct EffectTaskLauncher<S: TaskSpecialization> {
    specialization: S,
    host: Arc<S::Host>,
    exposes_exit: bool,
}

impl<S: TaskSpecialization> ReflectionTaskLauncher for EffectTaskLauncher<S> {
    fn build(
        &self,
        context: EvalContext,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>> {
        let task = EffectTask::new_in_context_with_capabilities(
            effect,
            self.specialization.clone(),
            self.host.clone(),
            context,
            false,
            self.exposes_exit,
        )
        .map_err(TaskHalt::into_failure)?;
        Ok(match result_policy {
            ReflectionTaskResultPolicy::RequireUnit => Box::new(UnitEffectTask(
                task.asserting_unit_result(Arc::from("reflection annotation result")),
            )),
            ReflectionTaskResultPolicy::ReturnValue => Box::new(ValueEffectTask(task)),
        })
    }
}

/// Runs a task with standard effects and no specialization-owned requests.
pub fn run_standard(
    runtime: &EvaluationRuntime,
    effect: &PublicValue,
    host: Arc<dyn TaskHost<StandardEffects>>,
) -> Result<TaskOutcome, TaskHalt> {
    EffectRun::new(runtime, effect, StandardEffects, host).run()
}

#[cfg(test)]
mod root_inventory_tests {
    use super::*;

    fn assert_lifecycle_root_inventory(
        lifecycle: &EffectLifecycle,
        state: &EffectLifecycleState,
        status: &EffectLifecycleStatus,
        terminal: &EffectLifecycleTerminal,
    ) {
        let EffectLifecycle {
            inner,
            terminal: lifecycle_terminal,
        } = lifecycle;
        let _: &Arc<EffectLifecycleState> = inner;
        let _: &Option<EffectLifecycleTerminal> = lifecycle_terminal;

        let EffectLifecycleState {
            status: stored_status,
            changed,
        } = state;
        let _: &Mutex<EffectLifecycleStatus> = stored_status;
        let _: &Condvar = changed;

        match status {
            EffectLifecycleStatus::Launched
            | EffectLifecycleStatus::Blocked
            | EffectLifecycleStatus::Cancelled
            | EffectLifecycleStatus::Abandoned
            | EffectLifecycleStatus::Exited => {}
            EffectLifecycleStatus::Complete(value) => {
                let _: &PublicValue = value;
            }
            EffectLifecycleStatus::Failed(failure) | EffectLifecycleStatus::Killed(failure) => {
                let _: &TaskHalt = failure;
            }
        }

        let EffectLifecycleTerminal { runtime, publisher } = terminal;
        let _: &crate::runtime::EvaluationRuntimeId = runtime;
        let _: &TaskStatusPublisher = publisher;
    }

    fn assert_effect_run_root_inventory(
        scheduled: &ScheduledEffectRun,
        run: &EffectRun<StandardEffects>,
        launcher: &EffectTaskLauncher<StandardEffects>,
    ) {
        let ScheduledEffectRun {
            context,
            session,
            task,
        } = scheduled;
        let _: &EvalContext = context;
        let _: &Arc<Mutex<Option<Arc<EvaluationSession>>>> = session;
        let _: &EvaluationTaskHandle = task;

        let EffectRun {
            effect,
            specialization,
            host,
            runtime,
            result_policy,
            result_assertion_context,
            failure_context,
        } = run;
        let _: &PublicValue = effect;
        let _: &StandardEffects = specialization;
        let _: &Arc<<StandardEffects as TaskSpecialization>::Host> = host;
        let _: &Option<EvaluationRuntime> = runtime;
        let _: &EffectResultPolicy = result_policy;
        let _: &Option<Arc<str>> = result_assertion_context;
        let _: &Option<PublicValue> = failure_context;

        let EffectTaskLauncher {
            specialization,
            host,
            exposes_exit,
        } = launcher;
        let _: &StandardEffects = specialization;
        let _: &Arc<<StandardEffects as TaskSpecialization>::Host> = host;
        let _: &bool = exposes_exit;
    }

    #[test]
    fn lifecycle_root_inventory_is_complete() {
        let _: fn(
            &EffectLifecycle,
            &EffectLifecycleState,
            &EffectLifecycleStatus,
            &EffectLifecycleTerminal,
        ) = assert_lifecycle_root_inventory;
        let _: fn(
            &ScheduledEffectRun,
            &EffectRun<StandardEffects>,
            &EffectTaskLauncher<StandardEffects>,
        ) = assert_effect_run_root_inventory;
    }

    #[test]
    fn lifecycle_status_preserves_coordinator_failure_root_identity() {
        let values = crate::core::test_value_factory();
        let failure = RuntimeFailureRoot::new(
            &values,
            Arc::new(EvaluationFailure::emission(Value::Number(
                crate::number::Number::integer(42),
            ))),
        );
        let state = EffectLifecycleState {
            status: Mutex::new(EffectLifecycleStatus::Launched),
            changed: Condvar::new(),
        };

        for status in [
            EvaluationTaskStatus::Failed(failure.clone()),
            EvaluationTaskStatus::Killed(failure.clone()),
        ] {
            let halt = match state.public_status(status) {
                EffectLifecycleStatus::Failed(halt) | EffectLifecycleStatus::Killed(halt) => halt,
                _ => panic!("a terminal failure should remain a failure"),
            };
            let published = halt
                .failure_root()
                .expect("the public lifecycle failure should retain its runtime root");
            assert!(published.shares_root_with(&failure));
        }
    }
}
