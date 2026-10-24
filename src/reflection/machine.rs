use super::promises::{PromiseId, Promises, Resolution};
use super::volume_handle::handle_volume;
use crate::trusted_hash::TrustedHashMap;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::protocol::{
    CommitResult, EffectRequestSpec, RequestActivity, RequestContext, RequestResult,
    SpecializationRequestInput, SpecializationRequestPoll, SpecializationRequestWork, TaskCommit,
    TaskHalt, TaskHost, TaskOutcome, TaskSpecialization, Transaction, ValidationResult,
    request_value,
};
use super::search::{IsolatedSearchBranch, SearchPolicy};
use super::store::{StoreJournal, StoreSnapshot, VolumeId, contents_at_path};
use crate::api::{EvaluatedValue, Value as PublicValue, Values};
use crate::core::{
    Atom, Builtin, BuiltinCall, CoreValueFactory, Dict, EvaluationFailure, FunctionValue, Key,
    LazyValue, List, ManagedResolverRoot, NetValue, PromisedValue, RuntimeValueAccess, Value,
    VolumeCellView, VolumeContentsView, keys,
};
use crate::core_net::{CoreDataKey, CoreSpecialization};
use crate::eval;
use crate::eval::whnf::WhnfComputation;
#[cfg(test)]
use crate::evaluation::OwnedEvalContext;
use crate::evaluation::{
    EvalContext, EvaluationExitBlock, EvaluationMachinePoll, EvaluationPollContext,
    EvaluationPumpOutcome, EvaluationSession, EvaluationTaskBlock, EvaluationTaskId,
    EvaluationTaskMachine, EvaluationValueAccess, EvaluationWaitPoll, EvaluatorStepContext,
    ExactDemandRoute, ExitIntent, WhnfOwnerPoll, WorkDependency, poll_whnf_computation,
};
use crate::interaction_net::NetBuilder;
use crate::number::Number;
use crate::runtime::RuntimeValueRoot;

#[cfg(test)]
use super::protocol::HostSnapshot;

mod reset_stack;

use reset_stack::{ResetStackMachine, ResetStackPoll};

/// A captured continuation's counter id within one reflection task, as a
/// trusted map key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ContinuationKey(u64);

#[derive(Clone)]
struct Tags {
    r: Key,
    seq: Key,
    alt: Key,
    fail: Key,
    cut: Key,
    trying: Key,
    eval: Key,
    fix: Key,
    get: Key,
    set: Key,
    try_get: Key,
    del: Key,
    heap_get: Key,
    heap_set: Key,
    heap_rewrite: Key,
    heap_try_get: Key,
    heap_del: Key,
    heap_vol: Key,
    vol_new: Key,
    vol_get: Key,
    vol_set: Key,
    vol_rewrite: Key,
    vol_try_get: Key,
    vol_del: Key,
    promise_new: Key,
    promise_status: Key,
    promise_fulfill: Key,
    promise_break: Key,
    reset: Key,
    shift: Key,
    resume: Key,
    exit_success: Key,
    exit_error: Key,
    continuation_state: Key,
}

impl Tags {
    fn new() -> Self {
        let tag = |name| {
            Key::atom_from_key(&Key::abstract_global_path([
                "reflection_runtime",
                "v0",
                "request",
                name,
            ]))
        };
        Self {
            r: tag("r"),
            seq: tag("seq"),
            alt: tag("alt"),
            fail: tag("fail"),
            cut: tag("cut"),
            trying: tag("try"),
            eval: tag("eval"),
            fix: tag("fix"),
            get: tag("get"),
            set: tag("set"),
            try_get: tag("try_get"),
            del: tag("del"),
            heap_get: tag("heap_get"),
            heap_set: tag("heap_set"),
            heap_rewrite: tag("heap_rewrite"),
            heap_try_get: tag("heap_try_get"),
            heap_del: tag("heap_del"),
            heap_vol: tag("heap_vol"),
            vol_new: tag("vol_new"),
            vol_get: tag("vol_get"),
            vol_set: tag("vol_set"),
            vol_rewrite: tag("vol_rewrite"),
            vol_try_get: tag("vol_try_get"),
            vol_del: tag("vol_del"),
            promise_new: tag("promise_new"),
            promise_status: tag("promise_status"),
            promise_fulfill: tag("promise_fulfill"),
            promise_break: tag("promise_break"),
            reset: tag("reset"),
            shift: tag("shift"),
            resume: tag("resume"),
            exit_success: tag("exit_success"),
            exit_error: tag("exit_error"),
            // The key is private, but its value deliberately travels with
            // whole-user-state get/set operations.
            continuation_state: Key::abstract_global_path([
                "reflection_runtime",
                "v0",
                "state",
                "continuations",
            ]),
        }
    }
}

pub(super) struct EffectTask<S: TaskSpecialization> {
    pub(super) eval_context: EvalContext,
    _demand_owner: Option<Arc<EvaluationSession>>,
    id: EvaluationTaskId,
    specialization: S,
    host: Arc<S::Host>,
    tags: Tags,
    specialized_requests: Vec<SpecializedRequest<S::Request>>,
    api: RuntimeValueRoot,
    next_continuation: u64,
    next_control_order: usize,
    continuations: TrustedHashMap<ContinuationKey, CapturedContinuation>,
    search: SearchPolicy<Branch<S>, IsolatedSearchBranch<S>>,
    execution: TaskExecution<S>,
    blocked: Option<BlockedExecution<S>>,
    exit: Option<TaskExitState<S>>,
    terminal: Option<TaskTerminal>,
    exact_demand_route: ExactDemandRoute,
    /// Where the task's host runs its cuts, what it tells the host.
    hosted: Option<HostedCuts<S>>,
    #[cfg(test)]
    phase_probe: Option<Arc<EffectPhaseProbe>>,
    #[cfg(test)]
    force_unfused: bool,
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum EffectMachinePhase {
    RequestParsed = 1,
    InterpreterEntered = 2,
    ContinuationDelivered = 3,
}

#[cfg(test)]
#[derive(Default)]
struct EffectPhaseProbe {
    phase: AtomicUsize,
    fused_requests: AtomicUsize,
    parsed_requests: AtomicUsize,
    dispatched_requests: AtomicUsize,
    application_starts: AtomicUsize,
}

#[cfg(test)]
impl EffectPhaseProbe {
    fn record(&self, phase: EffectMachinePhase) {
        match phase {
            EffectMachinePhase::RequestParsed => {
                self.parsed_requests.fetch_add(1, Ordering::AcqRel);
            }
            EffectMachinePhase::InterpreterEntered => {
                self.dispatched_requests.fetch_add(1, Ordering::AcqRel);
            }
            EffectMachinePhase::ContinuationDelivered => {}
        }
        let target = phase as usize;
        let previous = target - 1;
        let current = self.phase.load(Ordering::Acquire);
        if current >= target {
            return;
        }
        assert_eq!(
            current, previous,
            "effect-machine phases must cross their explicit boundary in order"
        );
        self.phase
            .compare_exchange(previous, target, Ordering::AcqRel, Ordering::Acquire)
            .expect("single-threaded effect polling must publish each phase once");
    }

    fn phase(&self) -> usize {
        self.phase.load(Ordering::Acquire)
    }

    fn record_fused_request(&self) {
        self.fused_requests.fetch_add(1, Ordering::AcqRel);
    }

    fn fused_requests(&self) -> usize {
        self.fused_requests.load(Ordering::Acquire)
    }

    fn parsed_requests(&self) -> usize {
        self.parsed_requests.load(Ordering::Acquire)
    }

    fn dispatched_requests(&self) -> usize {
        self.dispatched_requests.load(Ordering::Acquire)
    }

    fn record_application_start(&self) {
        self.application_starts.fetch_add(1, Ordering::AcqRel);
    }

    fn application_starts(&self) -> usize {
        self.application_starts.load(Ordering::Acquire)
    }
}

impl<S: TaskSpecialization> EffectTask<S> {
    #[cfg(test)]
    fn new(
        values: &CoreValueFactory,
        effect: Value,
        specialization: S,
        host: Arc<S::Host>,
    ) -> Result<Self, TaskHalt> {
        Self::new_owned_in_context(
            effect,
            specialization,
            host,
            EvalContext::isolated(values.clone()),
        )
    }

    #[cfg(test)]
    fn new_owned_in_context(
        effect: Value,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: OwnedEvalContext,
    ) -> Result<Self, TaskHalt> {
        let (eval_context, owner) = eval_context.into_parts();
        let effect = eval_context
            .values()
            .construct_runtime_value_root(|_| effect);
        Self::new_rooted_with_owner(effect, specialization, host, eval_context, owner)
    }

    #[cfg(test)]
    fn new_rooted(
        values: &CoreValueFactory,
        effect: RuntimeValueRoot,
        specialization: S,
        host: Arc<S::Host>,
    ) -> Result<Self, TaskHalt> {
        let (eval_context, owner) = EvalContext::isolated(values.clone()).into_parts();
        Self::new_rooted_with_owner(effect, specialization, host, eval_context, owner)
    }

    #[cfg(test)]
    fn new_rooted_with_owner(
        effect: RuntimeValueRoot,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
        owner: Arc<EvaluationSession>,
    ) -> Result<Self, TaskHalt> {
        let mut task = Self::new_in_context(effect, specialization, host, eval_context)?;
        task._demand_owner = Some(owner);
        Ok(task)
    }

    pub(super) fn new_in_context(
        effect: RuntimeValueRoot,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
    ) -> Result<Self, TaskHalt> {
        Self::new_in_context_with_policy(effect, specialization, host, eval_context, false)
    }

    pub(super) fn new_isolated_in_context(
        effect: RuntimeValueRoot,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
    ) -> Result<Self, TaskHalt> {
        Self::new_in_context_with_policy(effect, specialization, host, eval_context, true)
    }

    fn new_in_context_with_policy(
        effect: RuntimeValueRoot,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
        retain_all: bool,
    ) -> Result<Self, TaskHalt> {
        Self::new_in_context_with_capabilities(
            effect,
            specialization,
            host,
            eval_context,
            retain_all,
            false,
        )
    }

    pub(super) fn new_in_context_with_capabilities(
        effect: RuntimeValueRoot,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
        retain_all: bool,
        exposes_exit: bool,
    ) -> Result<Self, TaskHalt> {
        Self::new_rooted_in_context_with_capabilities(
            effect,
            specialization,
            host,
            eval_context,
            retain_all,
            exposes_exit,
        )
    }

    pub(super) fn new_rooted_in_context_with_capabilities(
        effect: RuntimeValueRoot,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
        retain_all: bool,
        exposes_exit: bool,
    ) -> Result<Self, TaskHalt> {
        Self::new_rooted_with_state(
            effect,
            None,
            specialization,
            host,
            eval_context,
            retain_all,
            exposes_exit,
        )
    }

    /// A task of `effect` that starts from `state`, whose host runs its
    /// cuts: the host keeps the effects of the task's requests, so the
    /// task reports where its cuts begin and end, and where it resumes a
    /// branch it set aside (`alt-cuts-its-first-operand`).
    pub(super) fn new_hosted_with_state_in_context(
        effect: RuntimeValueRoot,
        state: RuntimeValueRoot,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
    ) -> Result<Self, TaskHalt> {
        let mut task = Self::new_rooted_with_state(
            effect,
            Some(state),
            specialization,
            host,
            eval_context,
            false,
            false,
        )?;
        task.hosted = Some(HostedCuts::default());
        Ok(task)
    }

    /// A task of `effect` whose state starts as `state`, or `{}`.
    fn new_rooted_with_state(
        effect: RuntimeValueRoot,
        state: Option<RuntimeValueRoot>,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
        retain_all: bool,
        exposes_exit: bool,
    ) -> Result<Self, TaskHalt> {
        let eval_context = eval_context.for_effect_task();
        let tags = Tags::new();
        let (api, specialized_requests) = effect_api(
            eval_context.values(),
            &tags,
            specialization.requests(),
            specialization.serves_host_families(),
            specialization.exposes_shared_heap(),
            exposes_exit,
        )?;
        let id = eval_context
            .task_id()
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
        let initial_state = state.unwrap_or_else(|| {
            eval_context
                .values()
                .construct_runtime_value_root(|_| Value::Dict(Dict::new_sync()))
        });
        let root = Branch::new_rooted(effect, initial_state);
        let (search, branch) = if retain_all {
            let mut branch = root.clone();
            branch.transaction = Some(Transaction::new(host.snapshot()));
            (SearchPolicy::retaining_all(root), branch)
        } else {
            (SearchPolicy::FirstSuccess, root)
        };
        Ok(Self {
            eval_context,
            _demand_owner: None,
            id,
            specialization,
            host,
            tags,
            specialized_requests,
            api,
            next_continuation: 1,
            next_control_order: 1,
            continuations: TrustedHashMap::default(),
            search,
            execution: TaskExecution {
                work: MachineWork::Drive {
                    branch,
                    scope_depth: 0,
                },
                decoding: None,
                demanding: None,
                pathing: None,
                controlling: None,
                specializing: None,
                cuts: Vec::new(),
            },
            blocked: None,
            exit: None,
            terminal: None,
            exact_demand_route: ExactDemandRoute::default(),
            hosted: None,
            #[cfg(test)]
            phase_probe: None,
            #[cfg(test)]
            force_unfused: false,
        })
    }

    #[cfg(test)]
    fn new_exit_in_context(
        effect: Value,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
    ) -> Result<Self, TaskHalt> {
        let effect = eval_context
            .values()
            .construct_runtime_value_root(|_| effect);
        Self::new_in_context_with_capabilities(
            effect,
            specialization,
            host,
            eval_context,
            false,
            true,
        )
    }

    #[cfg(test)]
    fn with_phase_probe(mut self, probe: Arc<EffectPhaseProbe>) -> Self {
        self.phase_probe = Some(probe);
        self
    }

    #[cfg(test)]
    fn forcing_unfused(mut self) -> Self {
        self.force_unfused = true;
        self
    }

    #[cfg(test)]
    fn record_phase(&self, phase: EffectMachinePhase) {
        if let Some(probe) = &self.phase_probe {
            probe.record(phase);
        }
    }

    fn fusion_enabled(&self) -> bool {
        #[cfg(test)]
        if self.force_unfused {
            return false;
        }
        true
    }

    fn record_fused_request(&self) {
        #[cfg(test)]
        if let Some(probe) = &self.phase_probe {
            probe.record_fused_request();
        }
    }

    pub(super) fn completed_search(&self) -> Option<Arc<[IsolatedSearchBranch<S>]>> {
        self.search.completed()
    }

    pub(super) fn requiring_unit_result(mut self) -> Self {
        self.execution
            .work
            .branch_mut()
            .expect("a fresh effect task must contain its initial branch")
            .control
            .sequence
            .push(Continuation::RequireUnit);
        self
    }

    pub(super) fn asserting_unit_result(mut self, diagnostic_context: Arc<str>) -> Self {
        let values = self.eval_context.values().clone();
        let branch = self
            .execution
            .work
            .branch_mut()
            .expect("a fresh effect task must contain its initial branch");
        let diagnostic_context = values.with_boundary_access(|access| {
            branch.root_value(&access, Value::binary_from_text(&diagnostic_context))
        });
        branch
            .control
            .sequence
            .push(Continuation::AssertUnit(diagnostic_context));
        self
    }

    fn allocate_control_order(&mut self) -> Result<usize, TaskHalt> {
        let order = self.next_control_order;
        self.next_control_order = self
            .next_control_order
            .checked_add(1)
            .ok_or_else(|| TaskHalt::new("reflection control order exhausted"))?;
        Ok(order)
    }

    fn capture_continuation(
        &mut self,
        context: &EvaluationPollContext,
        continuation: CapturedContinuation,
    ) -> Result<RuntimeValueRoot, TaskHalt> {
        let id = self.next_continuation;
        self.next_continuation = self
            .next_continuation
            .checked_add(1)
            .ok_or_else(|| TaskHalt::new("reflection continuation IDs exhausted"))?;
        self.continuations.insert(ContinuationKey(id), continuation);
        Ok(context.with_runtime_access(|access| {
            access.root_runtime_value(request_function_in(
                &access,
                self.tags.resume.clone(),
                3,
                vec![
                    Value::Number(Number::from_u64(self.id.get())),
                    Value::Number(Number::from_u64(id)),
                ],
                true,
            ))
        }))
    }

    fn restart_fixpoint_at_scope(
        &mut self,
        context: &EvaluationPollContext,
        branch: &mut Branch<S>,
        scope_depth: usize,
    ) -> Result<Option<ControlWork<S>>, TaskHalt> {
        let Some(restart) = branch.fix_restarts.last() else {
            return Ok(None);
        };
        if restart.root.scope_depth < scope_depth {
            return Ok(None);
        }
        if restart.root.scope_depth > scope_depth {
            return Err(TaskHalt::new(
                "reflection fixpoint restart escaped its evaluation scope",
            ));
        }

        let restart = branch
            .fix_restarts
            .pop()
            .expect("restart observed above must exist");
        self.hosted_resume(restart.root.entry.host_version);
        let stack = context.evaluate(&self.eval_context, |evaluator| {
            reset_stack_root_in(
                evaluator,
                &restart.root.entry.state,
                &self.tags.continuation_state,
            )
        })?;
        Ok(Some(ControlWork::start_fixpoint_with_restarts(
            stack,
            restart.root,
            restart.choices,
            restart.inherited_restarts,
        )))
    }

    /// Runs this task to its outcome. The run drives the task as a scheduler
    /// driver does: it holds the thread's mutator across its polls and pumps,
    /// and suspends it to wait or to settle (`driver-mutator`).
    pub(super) fn run(&mut self) -> Result<TaskOutcome, TaskHalt> {
        let values = self.eval_context.values().clone();
        values.with_runtime_value_mutator(|mut mutator| loop {
            match self.poll_under(&mut mutator, 256) {
                EffectTaskPoll::BudgetExhausted => {}
                EffectTaskPoll::Blocked(blocked) => {
                    if let Some(dependency) = blocked.dependency {
                        // A panicked dependency would otherwise be pumped
                        // forever. A panic is never a task failure, so the
                        // run ends without assigning its promises.
                        if let Some(report) = dependency.panic_report() {
                            return Err(TaskHalt::panicked(report));
                        }
                        let wait = match dependency {
                            WorkDependency::Wait(wait) => wait,
                            WorkDependency::Promise(promise) => {
                                eval::promise_root_wait(&self.eval_context, &promise)
                                    .map_err(|error| TaskHalt::new(error.as_ref()))?
                            }
                            #[cfg(test)]
                            WorkDependency::Test(_) => {
                                return Err(TaskHalt::new(
                                    "synchronous reflection task cannot wait on a synthetic dependency",
                                ));
                            }
                        };
                        match self.eval_context.pump_wait_on_route_under(
                            &mut mutator,
                            &wait,
                            4_096,
                            &mut self.exact_demand_route,
                        ) {
                            EvaluationPumpOutcome::TargetReady
                            | EvaluationPumpOutcome::BudgetExhausted => continue,
                            EvaluationPumpOutcome::Busy => {
                                mutator.suspend(|| {
                                    self.eval_context.wait_for_claimed_task_on_route(
                                        &wait,
                                        &mut self.exact_demand_route,
                                    );
                                });
                                continue;
                            }
                            EvaluationPumpOutcome::NoProgress
                                if blocked.observed_generation.is_none() =>
                            {
                                if mutator.suspend(|| {
                                    self.eval_context
                                        .wait_for_observed_dependency_progress_on_route(
                                            &wait,
                                            &mut self.exact_demand_route,
                                        )
                                }) {
                                    continue;
                                }
                                let error = mutator.with_access(|access| {
                                    TaskHalt::new(
                                        "synchronous reflection task has no runnable producer for its dependency",
                                    )
                                    .root_in(&access)
                                });
                                self.finish_under(&mutator, TaskTerminal::Failed(error.clone()));
                                return Err(error);
                            }
                            EvaluationPumpOutcome::NoProgress => {}
                        }
                    }
                    let generation = blocked.observed_generation.ok_or_else(|| {
                        TaskHalt::new("blocked reflection task has no wake condition")
                    })?;
                    if !mutator.suspend(|| self.host.wait_for_change(generation)) {
                        self.finish_under(&mutator, TaskTerminal::Cancelled);
                    }
                }
                EffectTaskPoll::Complete(value) => return Ok(TaskOutcome::Complete(value)),
                EffectTaskPoll::Failed(error) => return Err(error),
                EffectTaskPoll::Cancelled => return Ok(TaskOutcome::Cancelled),
                EffectTaskPoll::Paused(_) => unreachable!("only a task whose host runs its cuts pauses"),
                EffectTaskPoll::Exit(_) => {
                    unreachable!("direct effect-run profiles do not expose runtime exit")
                }
            }
        })
    }

    /// Polls as [`Self::poll`] does, under a driver's `mutator`.
    fn poll_under(
        &mut self,
        mutator: &mut crate::core::RuntimeValueMutator<'_, '_>,
        steps: usize,
    ) -> EffectTaskPoll {
        let mut budget = crate::evaluation::EvaluationStepBudget::new(steps.max(1));
        let eval_context = self.eval_context.clone();
        let poll =
            EvaluationPollContext::quantum_for_context_under(mutator, &eval_context, |context| {
                self.poll_pumping(context, &mut budget)
            });
        crate::evaluation::assert_spent_when_exhausted(
            matches!(poll, EffectTaskPoll::BudgetExhausted),
            &budget,
        );
        poll
    }

    /// Polls this task within one total budget of `steps`, including the work
    /// it pumps while waiting, so a call never spends more than `steps`.
    pub(super) fn poll(&mut self, steps: usize) -> EffectTaskPoll {
        let mut budget = crate::evaluation::EvaluationStepBudget::new(steps.max(1));
        let eval_context = self.eval_context.clone();
        let poll = EvaluationPollContext::quantum_for_context(&eval_context, |context| {
            self.poll_pumping(context, &mut budget)
        });
        crate::evaluation::assert_spent_when_exhausted(
            matches!(poll, EffectTaskPoll::BudgetExhausted),
            &budget,
        );
        poll
    }

    /// Polls within `budget`, pumping a dependency it blocks on within the
    /// same budget rather than returning to its scheduler, so a demanded
    /// lazy is forced by this poll.
    pub(super) fn poll_pumping(
        &mut self,
        context: &mut EvaluationPollContext,
        budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EffectTaskPoll {
        while budget.remaining() != 0 {
            let poll = self.poll_with_context(context, budget);
            let EffectTaskPoll::Blocked(blocked) = &poll else {
                return poll;
            };
            if let Some(error) = blocked
                .dependency
                .as_ref()
                .and_then(|dependency| self.eval_context.recursive_promise_dependency(dependency))
            {
                let error =
                    context.with_runtime_access(|access| TaskHalt::new(error).root_in(&access));
                self.finish_in(context, TaskTerminal::Failed(error.clone()));
                return EffectTaskPoll::Failed(error);
            }
            // A panicked dependency stays blocked, so the scheduler's poll
            // boundary halts this task. The pump would find its wait ended
            // and poll again, which would block again.
            let Some(WorkDependency::Wait(wait)) = blocked
                .dependency
                .as_ref()
                .filter(|dependency| dependency.panic_report().is_none())
            else {
                return poll;
            };
            // Pumped work shares this poll's mutator.
            let pumped = self.eval_context.pump_wait_on_route_within(
                context,
                wait,
                budget,
                &mut self.exact_demand_route,
            );
            match pumped {
                EvaluationPumpOutcome::TargetReady => {}
                EvaluationPumpOutcome::BudgetExhausted => return EffectTaskPoll::BudgetExhausted,
                EvaluationPumpOutcome::Busy | EvaluationPumpOutcome::NoProgress => {
                    if let Some(error) = blocked.dependency.as_ref().and_then(|dependency| {
                        self.eval_context.recursive_promise_dependency(dependency)
                    }) {
                        let error = context
                            .with_runtime_access(|access| TaskHalt::new(error).root_in(&access));
                        self.finish_in(context, TaskTerminal::Failed(error.clone()));
                        return EffectTaskPoll::Failed(error);
                    }
                    // What made no progress may rest on a promise a `.try`
                    // answers with.
                    if matches!(pumped, EvaluationPumpOutcome::NoProgress)
                        && self.try_answers_wait(context)
                    {
                        continue;
                    }
                    return poll;
                }
            }
        }
        EffectTaskPoll::BudgetExhausted
    }

    pub(super) fn poll_with_context(
        &mut self,
        context: &mut EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EffectTaskPoll {
        context.assert_context(&self.eval_context);
        if let Some(terminal) = &self.terminal {
            return terminal.poll();
        }
        if let Some(poll) = self.poll_exit(context) {
            return poll;
        }
        if let Some(blocked) = self.poll_blocked(context, step_budget) {
            return blocked;
        }

        while step_budget.remaining() != 0 {
            if let Some(poll) = self.hosted_pause() {
                return poll;
            }
            // Each pass takes exactly one step of one phase.
            #[cfg(feature = "glam-prof")]
            self.eval_context
                .values()
                .record_reduction(crate::profiling::EvaluationReduction::ReflectionStep);
            if let Some(controlling) = self.execution.controlling.take() {
                match self.control_step(context, controlling, step_budget) {
                    ControlStep::Continue(controlling) => {
                        self.execution.controlling = Some(controlling);
                    }
                    ControlStep::Complete(work) => self.execution.work = work,
                    ControlStep::Blocked(controlling, dependency) => {
                        self.execution.controlling = Some(controlling);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked_in(context, blocked) {
                            return poll;
                        }
                    }
                    ControlStep::BudgetExhausted(controlling) => {
                        self.execution.controlling = Some(controlling);
                        return EffectTaskPoll::BudgetExhausted;
                    }
                    ControlStep::Failed(controlling, error) => {
                        self.execution.controlling = Some(controlling);
                        if let Some(poll) = self.handle_step_error(context, error) {
                            return poll;
                        }
                    }
                }
                // A phase step that advanced pays one unit as it ends,
                // forgiven when its delegations spent the last.
                let _ = step_budget.try_consume();
                continue;
            }
            if let Some(specializing) = self.execution.specializing.take() {
                match self.specialization_step(context, specializing, step_budget) {
                    SpecializationStep::Continue(specializing) => {
                        self.execution.specializing = Some(specializing);
                    }
                    SpecializationStep::Complete(work) => self.execution.work = *work,
                    SpecializationStep::Blocked(specializing, dependency) => {
                        self.execution.specializing = Some(specializing);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked_in(context, blocked) {
                            return poll;
                        }
                    }
                    SpecializationStep::BudgetExhausted(specializing) => {
                        self.execution.specializing = Some(specializing);
                        return EffectTaskPoll::BudgetExhausted;
                    }
                    SpecializationStep::Failed(specializing, error) => {
                        self.execution.specializing = Some(specializing);
                        if let Some(poll) = self.handle_step_error(context, error) {
                            return poll;
                        }
                    }
                }
                // A phase step that advanced pays one unit as it ends,
                // forgiven when its delegations spent the last.
                let _ = step_budget.try_consume();
                continue;
            }
            if let Some(demanding) = self.execution.demanding.take() {
                match self.scalar_demand_step(context, demanding, step_budget) {
                    ScalarDemandStep::Complete(step) => match step {
                        MachineStep::Continue(work) => self.execution.work = work,
                        MachineStep::Decode(decoding) => {
                            self.execution.work = MachineWork::Outcome {
                                outcome: BranchOutcome::Cancelled,
                                scope_depth: 0,
                            };
                            self.execution.decoding = Some(decoding);
                        }
                        MachineStep::Demand(_) => {
                            unreachable!("one scalar completion cannot immediately demand another")
                        }
                        MachineStep::StatePath(_) => {
                            unreachable!("one scalar completion cannot begin a state path")
                        }
                        MachineStep::Control(_) => {
                            unreachable!("one scalar completion cannot begin control work")
                        }
                        MachineStep::Specialize(_) => {
                            unreachable!("one scalar completion cannot begin specialized work")
                        }
                        MachineStep::Blocked(blocked) => {
                            if let Some(poll) = self.install_blocked_in(context, blocked) {
                                return poll;
                            }
                        }
                        MachineStep::Exit(intent) => {
                            let exit = self.prepare_exit(intent);
                            let poll = exit.poll.clone();
                            self.exit = Some(exit);
                            return EffectTaskPoll::Exit(poll);
                        }
                        MachineStep::Terminal(terminal) => {
                            self.finish_in(context, terminal);
                            return self.terminal.as_ref().expect("terminal set above").poll();
                        }
                    },
                    ScalarDemandStep::Blocked(demanding, dependency) => {
                        self.execution.demanding = Some(demanding);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked_in(context, blocked) {
                            return poll;
                        }
                    }
                    ScalarDemandStep::BudgetExhausted(demanding) => {
                        self.execution.demanding = Some(demanding);
                        return EffectTaskPoll::BudgetExhausted;
                    }
                    ScalarDemandStep::Failed(demanding, error) => {
                        self.execution.demanding = Some(demanding);
                        if let Some(poll) = self.handle_step_error(context, error) {
                            return poll;
                        }
                    }
                }
                // A phase step that advanced pays one unit as it ends,
                // forgiven when its delegations spent the last.
                let _ = step_budget.try_consume();
                continue;
            }
            if let Some(pathing) = self.execution.pathing.take() {
                match self.state_path_step(context, pathing, step_budget) {
                    StatePathStep::Continue(pathing) => self.execution.pathing = Some(pathing),
                    StatePathStep::Complete(work) => self.execution.work = work,
                    StatePathStep::Blocked(pathing, dependency) => {
                        self.execution.pathing = Some(pathing);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked_in(context, blocked) {
                            return poll;
                        }
                    }
                    StatePathStep::BudgetExhausted(pathing) => {
                        self.execution.pathing = Some(pathing);
                        return EffectTaskPoll::BudgetExhausted;
                    }
                    StatePathStep::Failed(pathing, error) => {
                        self.execution.pathing = Some(pathing);
                        if let Some(poll) = self.handle_step_error(context, error) {
                            return poll;
                        }
                    }
                }
                // A phase step that advanced pays one unit as it ends,
                // forgiven when its delegations spent the last.
                let _ = step_budget.try_consume();
                continue;
            }
            if let Some(decoding) = self.execution.decoding.take() {
                match self.decode_step(context, decoding, step_budget) {
                    EffectDecodeStep::Continue(decoding) => {
                        self.execution.decoding = Some(decoding);
                    }
                    EffectDecodeStep::Complete(work) => self.execution.work = work,
                    EffectDecodeStep::Blocked(decoding, dependency) => {
                        self.execution.decoding = Some(decoding);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked_in(context, blocked) {
                            return poll;
                        }
                    }
                    EffectDecodeStep::BudgetExhausted(decoding) => {
                        self.execution.decoding = Some(decoding);
                        return EffectTaskPoll::BudgetExhausted;
                    }
                    EffectDecodeStep::Failed(decoding, error) => {
                        self.execution.decoding = Some(decoding);
                        if let Some(poll) = self.handle_step_error(context, error) {
                            return poll;
                        }
                    }
                }
                // A phase step that advanced pays one unit as it ends,
                // forgiven when its delegations spent the last.
                let _ = step_budget.try_consume();
                continue;
            }
            assert!(step_budget.try_consume());
            let work = self.take_work_for_step();
            match self.step(context, work) {
                Ok(MachineStep::Continue(work)) => self.execution.work = work,
                Ok(MachineStep::Decode(decoding)) => {
                    self.execution.work = MachineWork::Outcome {
                        outcome: BranchOutcome::Cancelled,
                        scope_depth: 0,
                    };
                    self.execution.decoding = Some(decoding);
                }
                Ok(MachineStep::Demand(demanding)) => {
                    self.execution.work = MachineWork::Outcome {
                        outcome: BranchOutcome::Cancelled,
                        scope_depth: 0,
                    };
                    self.execution.demanding = Some(demanding);
                }
                Ok(MachineStep::StatePath(pathing)) => {
                    self.execution.work = MachineWork::Outcome {
                        outcome: BranchOutcome::Cancelled,
                        scope_depth: 0,
                    };
                    self.execution.pathing = Some(*pathing);
                }
                Ok(MachineStep::Control(controlling)) => {
                    self.execution.work = MachineWork::Outcome {
                        outcome: BranchOutcome::Cancelled,
                        scope_depth: 0,
                    };
                    self.execution.controlling = Some(*controlling);
                }
                Ok(MachineStep::Specialize(specializing)) => {
                    self.execution.work = MachineWork::Outcome {
                        outcome: BranchOutcome::Cancelled,
                        scope_depth: 0,
                    };
                    self.execution.specializing = Some(specializing);
                }
                Ok(MachineStep::Blocked(blocked)) => {
                    if let Some(poll) = self.install_blocked_in(context, blocked) {
                        return poll;
                    }
                }
                Ok(MachineStep::Terminal(terminal)) => {
                    self.finish_in(context, terminal);
                    return self.terminal.as_ref().expect("terminal set above").poll();
                }
                Ok(MachineStep::Exit(intent)) => {
                    let exit = self.prepare_exit(intent);
                    let poll = exit.poll.clone();
                    self.exit = Some(exit);
                    return EffectTaskPoll::Exit(poll);
                }
                Err(error) => {
                    if let Some(poll) = self.handle_step_error(context, error) {
                        return poll;
                    }
                }
            }
        }
        EffectTaskPoll::BudgetExhausted
    }

    /// The work for one step. A failed step's recovery (`retry_wake`) reads
    /// the work it started from only through a transaction, a retry
    /// checkpoint, or a cut that owns a transaction; without those, the
    /// work moves into the step instead of being cloned each step.
    fn take_work_for_step(&mut self) -> MachineWork<S> {
        let recoverable = self
            .execution
            .cuts
            .iter()
            .any(|frame| frame.owns_transaction)
            || self
                .execution
                .work
                .branch()
                .is_some_and(|branch| branch.transaction.is_some() || branch.retry.is_some());
        if recoverable {
            self.execution.work.clone()
        } else {
            std::mem::replace(
                &mut self.execution.work,
                MachineWork::Outcome {
                    outcome: BranchOutcome::Cancelled,
                    scope_depth: 0,
                },
            )
        }
    }

    /// Handles a failed step: `None` when its retry restarted the task, which
    /// goes on within the poll.
    fn handle_step_error(
        &mut self,
        context: &mut EvaluationPollContext,
        error: TaskHalt,
    ) -> Option<EffectTaskPoll> {
        let error = context.with_runtime_access(|access| error.root_in(&access));
        // A `.try` answers with an evaluation error within it.
        if error.panic_message().is_none()
            && let Some(index) = self.innermost_try()
        {
            self.unwind_to_try(context, index, TryOutcome::Err(error), None);
            return None;
        }
        if let Some(retry) = self.retry_wake() {
            let blocked =
                BlockedExecution::evaluation_error(error, retry, self.eval_context.values());
            return self.install_blocked(context, blocked);
        }
        self.finish_in(context, TaskTerminal::Failed(error));
        Some(self.terminal.as_ref().expect("terminal set above").poll())
    }

    fn decode_step(
        &mut self,
        context: &EvaluationPollContext,
        mut decoding: EffectDecodeWork<S>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EffectDecodeStep<S> {
        if let EffectDecodeOperation::Request(request) = &mut decoding.operation {
            return match request.poll(
                context,
                &self.eval_context,
                &self.tags,
                &self.specialized_requests,
                step_budget,
            ) {
                RequestDecodePoll::Ready(request) => {
                    #[cfg(test)]
                    self.record_phase(EffectMachinePhase::RequestParsed);
                    EffectDecodeStep::Complete(MachineWork::Interpret {
                        request,
                        branch: decoding.branch,
                        scope_depth: decoding.scope_depth,
                    })
                }
                RequestDecodePoll::Continue => EffectDecodeStep::Continue(decoding),
                RequestDecodePoll::Pending(dependency) => {
                    EffectDecodeStep::Blocked(decoding, dependency)
                }
                RequestDecodePoll::BudgetExhausted => EffectDecodeStep::BudgetExhausted(decoding),
                RequestDecodePoll::Failed(error) => {
                    let error = decoding.contextualize(error, context, &self.eval_context);
                    EffectDecodeStep::Failed(decoding, error)
                }
            };
        }

        let EffectDecodeOperation::Whnf {
            computation,
            purpose,
        } = &mut decoding.operation
        else {
            unreachable!("request decoding returned above")
        };
        let purpose = *purpose;
        match poll_whnf_computation(computation, context, &self.eval_context, step_budget) {
            WhnfOwnerPoll::Pending(dependency) => {
                #[cfg(test)]
                if matches!(purpose, EffectDecodePurpose::RequestApplication)
                    && let WorkDependency::Wait(wait) = &dependency
                {
                    self.eval_context.pause_deferred_pump(wait);
                }
                EffectDecodeStep::Blocked(decoding, dependency)
            }
            WhnfOwnerPoll::BudgetExhausted => EffectDecodeStep::BudgetExhausted(decoding),
            WhnfOwnerPoll::Failed(failure) => {
                let error = decoding.contextualize(
                    TaskHalt::rooted_failure(failure),
                    context,
                    &self.eval_context,
                );
                EffectDecodeStep::Failed(decoding, error)
            }
            WhnfOwnerPoll::Ready(value) => {
                self.complete_decode_phase(context, decoding, purpose, value)
            }
        }
    }

    fn specialization_step(
        &mut self,
        context: &mut EvaluationPollContext,
        mut specializing: Box<SpecializationWork<S>>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> SpecializationStep<S> {
        if let Some(computation) = specializing.demand.as_mut() {
            let input = match poll_whnf_computation(
                computation,
                context,
                &self.eval_context,
                step_budget,
            ) {
                WhnfOwnerPoll::Ready(value) => {
                    SpecializationRequestInput::Value(context.with_runtime_access(|access| {
                        EvaluatedValue::from_whnf_in(&access, PublicValue::from_runtime_root(value))
                    }))
                }
                WhnfOwnerPoll::Pending(dependency) => {
                    return SpecializationStep::Blocked(specializing, dependency);
                }
                WhnfOwnerPoll::BudgetExhausted => {
                    return SpecializationStep::BudgetExhausted(specializing);
                }
                WhnfOwnerPoll::Failed(failure) => {
                    SpecializationRequestInput::Failed(TaskHalt::rooted_failure(failure))
                }
            };
            specializing.demand = None;
            specializing.input = Some(input);
        }

        let checkpoint = specializing.branch.retry_candidate();
        let mut activity = RequestActivity::default();
        // The request is the specialization's code, which may call its host:
        // one suspension covers starting it and polling it.
        let result = context.suspend(|| {
            let work = &mut *specializing;
            work.request.started(&self.specialization).poll(
                &self.specialization,
                work.input.take(),
                &mut RequestContext {
                    eval_context: &self.eval_context,
                    host: &self.host,
                    transaction: work.branch.transaction.as_mut(),
                    state: &mut work.branch.state,
                    activity: &mut activity,
                },
            )
        });
        if let Some(generation) = activity.observed_generation {
            specializing.branch.observe(checkpoint, generation);
        }
        if activity.committed {
            specializing.branch.retry = None;
        }
        let result = match result {
            Ok(result) => result,
            Err(error) => return SpecializationStep::Failed(specializing, error),
        };

        match result {
            SpecializationRequestPoll::Continue => SpecializationStep::Continue(specializing),
            SpecializationRequestPoll::Demand(value) => {
                let values = Values::from_core_factory(self.eval_context.values().clone());
                if let Err(error) = values.require(&value) {
                    return SpecializationStep::Failed(
                        specializing,
                        TaskHalt::new(error.to_string()),
                    );
                }
                specializing.demand = Some(WhnfComputation::from_root(value.into_runtime_root()));
                SpecializationStep::Continue(specializing)
            }
            SpecializationRequestPoll::Wait(wait) => {
                SpecializationStep::Blocked(specializing, WorkDependency::Wait(wait.into_inner()))
            }
            SpecializationRequestPoll::Complete(result) => {
                if let Err(error) = self.validate_specialization_result(&result) {
                    return SpecializationStep::Failed(specializing, error);
                }
                // The host answered the request, changing its state.
                let mut branch = specializing.branch;
                if let Some(hosted) = &mut self.hosted {
                    hosted.version += 1;
                    hosted.set_aside = false;
                    branch.host_version = hosted.version;
                }
                SpecializationStep::Complete(Box::new(self.complete_specialization_request(
                    context,
                    result,
                    branch,
                    specializing.scope_depth,
                )))
            }
        }
    }

    fn scalar_demand_step(
        &mut self,
        context: &EvaluationPollContext,
        mut demanding: ScalarDemandWork<S>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ScalarDemandStep<S> {
        let value = match poll_whnf_computation(
            &mut demanding.computation,
            context,
            &self.eval_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(value) => value,
            WhnfOwnerPoll::Pending(dependency) => {
                return ScalarDemandStep::Blocked(demanding, dependency);
            }
            WhnfOwnerPoll::BudgetExhausted => return ScalarDemandStep::BudgetExhausted(demanding),
            WhnfOwnerPoll::Failed(failure) => {
                return ScalarDemandStep::Failed(demanding, TaskHalt::rooted_failure(failure));
            }
        };
        let ScalarDemandWork {
            purpose,
            mut branch,
            scope_depth,
            ..
        } = demanding;
        let step = match purpose {
            ScalarDemandPurpose::ApplyContinuation { argument, fused } => {
                let Some(Continuation::Glam(_)) = branch.control.sequence.last() else {
                    return ScalarDemandStep::Failed(
                        ScalarDemandWork::new(
                            value,
                            ScalarDemandPurpose::ApplyContinuation { argument, fused },
                            branch,
                            scope_depth,
                        ),
                        TaskHalt::new("reflection continuation control became unbalanced"),
                    );
                };
                if fused {
                    self.record_fused_request();
                    branch.control.sequence.pop();
                    MachineStep::Decode(EffectDecodeWork::application(
                        context,
                        &self.eval_context,
                        value,
                        vec![argument],
                        EffectDecodePurpose::AppliedEffect,
                        branch,
                        scope_depth,
                    ))
                } else {
                    branch.control.sequence.pop();
                    MachineStep::Continue(MachineWork::apply_roots(
                        value,
                        vec![argument],
                        branch,
                        scope_depth,
                    ))
                }
            }
            ScalarDemandPurpose::RequireUnit => {
                let checked = context.evaluate(&self.eval_context, |evaluator| {
                    evaluator.project_root(&value, |access, value| {
                        if !access
                            .values()
                            .same_representation(&value, &access.values().unit())
                        {
                            return Err(TaskHalt::new(format!(
                                "effect task returned {}; expected unit",
                                access.values().diagnostic_kind_name(&value)
                            )));
                        }
                        Ok(())
                    })
                });
                if let Err(error) = checked {
                    return ScalarDemandStep::Failed(
                        ScalarDemandWork::new(
                            value,
                            ScalarDemandPurpose::RequireUnit,
                            branch,
                            scope_depth,
                        ),
                        error,
                    );
                }
                branch.control.sequence.pop();
                MachineStep::Continue(MachineWork::deliver_root(value, branch, scope_depth))
            }
            ScalarDemandPurpose::AssertUnit => {
                branch.control.sequence.pop();
                MachineStep::Continue(MachineWork::deliver_root(value, branch, scope_depth))
            }
            ScalarDemandPurpose::RestoreScopedValue { scoped_value } => {
                let checked = context.evaluate(&self.eval_context, |evaluator| {
                    evaluator.project_root(&value, |access, value| {
                        if !access
                            .values()
                            .same_representation(&value, &access.values().unit())
                        {
                            return Err(TaskHalt::new(format!(
                                "scoped effect close must return unit, got {:?}",
                                access.values().diagnostic_debug(&value)
                            )));
                        }
                        Ok(())
                    })
                });
                if let Err(error) = checked {
                    return ScalarDemandStep::Failed(
                        ScalarDemandWork::new(
                            value,
                            ScalarDemandPurpose::RestoreScopedValue { scoped_value },
                            branch,
                            scope_depth,
                        ),
                        error,
                    );
                }
                branch.control.sequence.pop();
                MachineStep::Continue(MachineWork::deliver_root(scoped_value, branch, scope_depth))
            }
            ScalarDemandPurpose::ExitError => MachineStep::Exit(ExitIntent::Error(value)),
        };
        ScalarDemandStep::Complete(step)
    }

    fn control_step(
        &mut self,
        context: &EvaluationPollContext,
        mut controlling: ControlWork<S>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ControlStep<S> {
        let operation = std::mem::replace(&mut controlling.operation, ControlOperation::Poisoned);
        match operation {
            ControlOperation::Key {
                mut key,
                stack,
                disposition,
            } => {
                let poll = context.evaluate(&self.eval_context, |evaluator| {
                    key.poll(context, evaluator, &self.eval_context, step_budget)
                });
                match poll {
                    eval::ConversionPoll::Ready(key) => {
                        controlling.operation = ControlOperation::Stack {
                            key,
                            stack,
                            disposition,
                        };
                        ControlStep::Continue(controlling)
                    }
                    eval::ConversionPoll::Pending(dependency) => {
                        controlling.operation = ControlOperation::Key {
                            key,
                            stack,
                            disposition,
                        };
                        ControlStep::Blocked(controlling, dependency)
                    }
                    eval::ConversionPoll::BudgetExhausted => {
                        controlling.operation = ControlOperation::Key {
                            key,
                            stack,
                            disposition,
                        };
                        ControlStep::BudgetExhausted(controlling)
                    }
                    eval::ConversionPoll::Failed(failure) => {
                        controlling.operation = ControlOperation::Key {
                            key,
                            stack,
                            disposition,
                        };
                        ControlStep::Failed(controlling, TaskHalt::rooted_failure(failure))
                    }
                }
            }
            ControlOperation::Stack {
                key,
                mut stack,
                disposition,
            } => match stack.poll(context, &self.eval_context, step_budget) {
                ResetStackPoll::Ready(decoded) => {
                    let reset_stack::DecodedResetStack {
                        serialized,
                        mut frames,
                    } = decoded;
                    drop(serialized);
                    let mut branch = controlling.branch;
                    let scope_depth = controlling.scope_depth;
                    match disposition {
                        KeyedControl::Reset { operation } => {
                            let order = match self.allocate_control_order() {
                                Ok(order) => order,
                                Err(error) => {
                                    return ControlStep::Failed(
                                        ControlWork::poisoned(branch, scope_depth),
                                        error,
                                    );
                                }
                            };
                            let continuation = match self.capture_continuation(
                                context,
                                CapturedContinuation {
                                    sequence: std::mem::take(&mut branch.control.sequence),
                                    delimiters: Vec::new(),
                                    reset_frames: Vec::new(),
                                },
                            ) {
                                Ok(continuation) => continuation,
                                Err(error) => {
                                    return ControlStep::Failed(
                                        ControlWork::poisoned(branch, scope_depth),
                                        error,
                                    );
                                }
                            };
                            frames.push(ResetFrame {
                                key,
                                continuation,
                                scope_depth,
                                order,
                            });
                            let state = context.evaluate(&self.eval_context, |evaluator| {
                                evaluator.project_root(&branch.state, |access, state| {
                                    encode_reset_frames_in_state(
                                        access,
                                        state,
                                        &self.tags.continuation_state,
                                        &frames,
                                    )
                                })
                            });
                            branch.state = state;
                            branch.set_effect_root(operation);
                            ControlStep::Complete(MachineWork::Drive {
                                branch,
                                scope_depth,
                            })
                        }
                        KeyedControl::Shift { function } => {
                            let Some(index) = frames.iter().rposition(|frame| frame.key == key)
                            else {
                                return ControlStep::Failed(
                                    ControlWork::poisoned(branch, scope_depth),
                                    TaskHalt::new("`.shift` key is not in reset scope"),
                                );
                            };
                            let inner_reset_frames = frames.split_off(index + 1);
                            let target = frames.pop().expect("matching reset frame must exist");
                            let first_inner_delimiter = branch
                                .control
                                .delimiters
                                .iter()
                                .position(|delimiter| delimiter.order() > target.order)
                                .unwrap_or(branch.control.delimiters.len());
                            let inner_delimiters =
                                branch.control.delimiters.split_off(first_inner_delimiter);
                            let continuation = match self.capture_continuation(
                                context,
                                CapturedContinuation {
                                    sequence: std::mem::take(&mut branch.control.sequence),
                                    delimiters: inner_delimiters,
                                    reset_frames: inner_reset_frames,
                                },
                            ) {
                                Ok(continuation) => continuation,
                                Err(error) => {
                                    return ControlStep::Failed(
                                        ControlWork::poisoned(branch, scope_depth),
                                        error,
                                    );
                                }
                            };
                            let state = context.evaluate(&self.eval_context, |evaluator| {
                                evaluator.project_root(&branch.state, |access, state| {
                                    encode_reset_frames_in_state(
                                        access,
                                        state,
                                        &self.tags.continuation_state,
                                        &frames,
                                    )
                                })
                            });
                            branch.state = state;
                            branch
                                .control
                                .sequence
                                .push(Continuation::Glam(target.continuation));
                            ControlStep::Complete(MachineWork::apply_roots(
                                function,
                                vec![continuation],
                                branch,
                                scope_depth,
                            ))
                        }
                    }
                }
                ResetStackPoll::Continue => {
                    controlling.operation = ControlOperation::Stack {
                        key,
                        stack,
                        disposition,
                    };
                    ControlStep::Continue(controlling)
                }
                ResetStackPoll::Pending(dependency) => {
                    controlling.operation = ControlOperation::Stack {
                        key,
                        stack,
                        disposition,
                    };
                    ControlStep::Blocked(controlling, dependency)
                }
                ResetStackPoll::BudgetExhausted => {
                    controlling.operation = ControlOperation::Stack {
                        key,
                        stack,
                        disposition,
                    };
                    ControlStep::BudgetExhausted(controlling)
                }
                ResetStackPoll::Failed(error) => {
                    controlling.operation = ControlOperation::Stack {
                        key,
                        stack,
                        disposition,
                    };
                    ControlStep::Failed(controlling, error)
                }
            },
            ControlOperation::InstallCaptured {
                mut stack,
                captured,
                value,
            } => match stack.poll(context, &self.eval_context, step_budget) {
                ResetStackPoll::Ready(decoded) => {
                    let mut layers = captured
                        .reset_frames
                        .iter()
                        .cloned()
                        .map(CapturedLayer::Reset)
                        .chain(
                            captured
                                .delimiters
                                .iter()
                                .cloned()
                                .map(CapturedLayer::Delimiter),
                        )
                        .collect::<Vec<_>>();
                    layers.sort_by_key(CapturedLayer::order);
                    let allocation_count = match layers.len().checked_add(1) {
                        Some(count) => count,
                        None => {
                            return ControlStep::Failed(
                                ControlWork::poisoned(controlling.branch, controlling.scope_depth),
                                TaskHalt::new("reflection control order exhausted"),
                            );
                        }
                    };
                    let next_order = match self.next_control_order.checked_add(allocation_count) {
                        Some(order) => order,
                        None => {
                            return ControlStep::Failed(
                                ControlWork::poisoned(controlling.branch, controlling.scope_depth),
                                TaskHalt::new("reflection control order exhausted"),
                            );
                        }
                    };
                    let resume_order = self.next_control_order;
                    let mut frames = decoded.frames;
                    let mut delimiters = Vec::new();
                    for (order, layer) in ((resume_order + 1)..).zip(layers) {
                        match layer {
                            CapturedLayer::Reset(mut frame) => {
                                frame.scope_depth = controlling.scope_depth;
                                frame.order = order;
                                frames.push(frame);
                            }
                            CapturedLayer::Delimiter(mut delimiter) => {
                                delimiter.rebase(controlling.scope_depth, order);
                                delimiters.push(delimiter);
                            }
                        }
                    }
                    let mut branch = controlling.branch;
                    let caller_sequence = std::mem::take(&mut branch.control.sequence);
                    branch.control.delimiters.push(Delimiter::Resume {
                        outer_sequence: caller_sequence,
                        scope_depth: controlling.scope_depth,
                        order: resume_order,
                    });
                    let state = context.evaluate(&self.eval_context, |evaluator| {
                        evaluator.project_root(&branch.state, |access, state| {
                            encode_reset_frames_in_state(
                                access,
                                state,
                                &self.tags.continuation_state,
                                &frames,
                            )
                        })
                    });
                    self.next_control_order = next_order;
                    branch.state = state;
                    branch.control.delimiters.extend(delimiters);
                    branch.control.sequence = captured.sequence;
                    ControlStep::Complete(MachineWork::deliver_root(
                        value,
                        branch,
                        controlling.scope_depth,
                    ))
                }
                ResetStackPoll::Continue => {
                    controlling.operation = ControlOperation::InstallCaptured {
                        stack,
                        captured,
                        value,
                    };
                    ControlStep::Continue(controlling)
                }
                ResetStackPoll::Pending(dependency) => {
                    controlling.operation = ControlOperation::InstallCaptured {
                        stack,
                        captured,
                        value,
                    };
                    ControlStep::Blocked(controlling, dependency)
                }
                ResetStackPoll::BudgetExhausted => {
                    controlling.operation = ControlOperation::InstallCaptured {
                        stack,
                        captured,
                        value,
                    };
                    ControlStep::BudgetExhausted(controlling)
                }
                ResetStackPoll::Failed(error) => {
                    controlling.operation = ControlOperation::InstallCaptured {
                        stack,
                        captured,
                        value,
                    };
                    ControlStep::Failed(controlling, error)
                }
            },
            ControlOperation::StartFixpoint {
                mut stack,
                root,
                choices,
            } => match stack.poll(context, &self.eval_context, step_budget) {
                ResetStackPoll::Ready(decoded) => {
                    let reset_stack::DecodedResetStack {
                        serialized: reset_stack,
                        frames: _,
                    } = decoded;
                    let mut branch = controlling.branch;
                    let state = context.evaluate(&self.eval_context, |evaluator| {
                        evaluator.project_root(&branch.state, |access, state| {
                            encode_reset_frames_in_state(
                                access,
                                state,
                                &self.tags.continuation_state,
                                &[],
                            )
                        })
                    });
                    let order = match self.allocate_control_order() {
                        Ok(order) => order,
                        Err(error) => {
                            return ControlStep::Failed(
                                ControlWork::poisoned(branch, root.scope_depth),
                                error,
                            );
                        }
                    };
                    let handle = match context.with_runtime_access(|access| {
                        PromisedValue::fixpoint(&access, &self.eval_context)
                    }) {
                        Ok(handle) => handle,
                        Err(error) => {
                            return ControlStep::Failed(
                                ControlWork::poisoned(branch, root.scope_depth),
                                TaskHalt::new(error.as_ref()),
                            );
                        }
                    };
                    let marker = context.with_runtime_access(|access| {
                        branch.root_value(&access, Value::Promised(handle.duplicate_in(&access)))
                    });
                    let outer_control = std::mem::take(&mut branch.control);
                    branch.state = state;
                    let handle = context.with_runtime_access(|access| handle.root_in(&access));
                    branch.active_fixes.push(ActiveFix {
                        root: root.clone(),
                        choices,
                        next_choice: 0,
                        handle: handle.clone(),
                    });
                    branch.control.sequence.push(Continuation::Fix(handle));
                    branch.control.delimiters.push(Delimiter::Restore {
                        outer: Box::new(outer_control),
                        reset_stack,
                        scope_depth: root.scope_depth,
                        order,
                    });
                    ControlStep::Complete(MachineWork::apply_roots(
                        root.function.clone(),
                        vec![marker],
                        branch,
                        root.scope_depth,
                    ))
                }
                ResetStackPoll::Continue => {
                    controlling.operation = ControlOperation::StartFixpoint {
                        stack,
                        root,
                        choices,
                    };
                    ControlStep::Continue(controlling)
                }
                ResetStackPoll::Pending(dependency) => {
                    controlling.operation = ControlOperation::StartFixpoint {
                        stack,
                        root,
                        choices,
                    };
                    ControlStep::Blocked(controlling, dependency)
                }
                ResetStackPoll::BudgetExhausted => {
                    controlling.operation = ControlOperation::StartFixpoint {
                        stack,
                        root,
                        choices,
                    };
                    ControlStep::BudgetExhausted(controlling)
                }
                ResetStackPoll::Failed(error) => {
                    controlling.operation = ControlOperation::StartFixpoint {
                        stack,
                        root,
                        choices,
                    };
                    ControlStep::Failed(controlling, error)
                }
            },
            ControlOperation::Delivery { mut stack, value } => {
                match stack.poll(context, &self.eval_context, step_budget) {
                    ResetStackPoll::Ready(decoded) => {
                        let mut resets = decoded.frames;
                        let mut branch = controlling.branch;
                        let scope_depth = controlling.scope_depth;
                        let reset_order = resets
                            .last()
                            .filter(|frame| frame.scope_depth >= scope_depth)
                            .map(|frame| frame.order);
                        let delimiter_order = branch
                            .control
                            .delimiters
                            .last()
                            .filter(|delimiter| delimiter.scope_depth() >= scope_depth)
                            .map(Delimiter::order);
                        if reset_order > delimiter_order {
                            let frame = resets.pop().expect("reset order came from a frame");
                            let state = context.evaluate(&self.eval_context, |evaluator| {
                                evaluator.project_root(&branch.state, |access, state| {
                                    encode_reset_frames_in_state(
                                        access,
                                        state,
                                        &self.tags.continuation_state,
                                        &resets,
                                    )
                                })
                            });
                            branch.state = state;
                            return ControlStep::Complete(MachineWork::apply_roots(
                                frame.continuation,
                                vec![value],
                                branch,
                                scope_depth,
                            ));
                        }
                        let Some(_) = delimiter_order else {
                            return ControlStep::Complete(MachineWork::Outcome {
                                outcome: BranchOutcome::Complete(value, branch),
                                scope_depth,
                            });
                        };
                        match branch
                            .control
                            .delimiters
                            .last()
                            .cloned()
                            .expect("delimiter order came from a delimiter")
                        {
                            Delimiter::Resume { outer_sequence, .. } => {
                                branch.control.delimiters.pop();
                                branch.control.sequence = outer_sequence;
                            }
                            Delimiter::Restore {
                                outer, reset_stack, ..
                            } => {
                                controlling.branch = branch;
                                controlling.operation = ControlOperation::Restore {
                                    stack: ResetStackMachine::new(reset_stack),
                                    outer,
                                    value,
                                };
                                return ControlStep::Continue(controlling);
                            }
                        }
                        ControlStep::Complete(MachineWork::deliver_root(value, branch, scope_depth))
                    }
                    ResetStackPoll::Continue => {
                        controlling.operation = ControlOperation::Delivery { stack, value };
                        ControlStep::Continue(controlling)
                    }
                    ResetStackPoll::Pending(dependency) => {
                        controlling.operation = ControlOperation::Delivery { stack, value };
                        ControlStep::Blocked(controlling, dependency)
                    }
                    ResetStackPoll::BudgetExhausted => {
                        controlling.operation = ControlOperation::Delivery { stack, value };
                        ControlStep::BudgetExhausted(controlling)
                    }
                    ResetStackPoll::Failed(error) => {
                        controlling.operation = ControlOperation::Delivery { stack, value };
                        ControlStep::Failed(controlling, error)
                    }
                }
            }
            ControlOperation::Restore {
                mut stack,
                outer,
                value,
            } => match stack.poll(context, &self.eval_context, step_budget) {
                ResetStackPoll::Ready(decoded) => {
                    let mut branch = controlling.branch;
                    let state = context.evaluate(&self.eval_context, |evaluator| {
                        evaluator.with_value_access(|access| {
                            let Value::Dict(state) = access.clone_root(&branch.state) else {
                                return Err(TaskHalt::new(
                                    "reflection user state must be a dictionary",
                                ));
                            };
                            let serialized = access.clone_root(&decoded.serialized);
                            Ok(access.root_value(Value::Dict(
                                state.insert(self.tags.continuation_state.clone(), serialized),
                            )))
                        })
                    });
                    let state = match state {
                        Ok(state) => state,
                        Err(error) => {
                            return ControlStep::Failed(
                                ControlWork::poisoned(branch, controlling.scope_depth),
                                error,
                            );
                        }
                    };
                    branch.control.delimiters.pop();
                    branch.state = state;
                    branch.control = *outer;
                    ControlStep::Complete(MachineWork::deliver_root(
                        value,
                        branch,
                        controlling.scope_depth,
                    ))
                }
                ResetStackPoll::Continue => {
                    controlling.operation = ControlOperation::Restore {
                        stack,
                        outer,
                        value,
                    };
                    ControlStep::Continue(controlling)
                }
                ResetStackPoll::Pending(dependency) => {
                    controlling.operation = ControlOperation::Restore {
                        stack,
                        outer,
                        value,
                    };
                    ControlStep::Blocked(controlling, dependency)
                }
                ResetStackPoll::BudgetExhausted => {
                    controlling.operation = ControlOperation::Restore {
                        stack,
                        outer,
                        value,
                    };
                    ControlStep::BudgetExhausted(controlling)
                }
                ResetStackPoll::Failed(error) => {
                    controlling.operation = ControlOperation::Restore {
                        stack,
                        outer,
                        value,
                    };
                    ControlStep::Failed(controlling, error)
                }
            },
            ControlOperation::Poisoned => {
                unreachable!("failed control work cannot be resumed before error handling")
            }
        }
    }

    fn state_path_step(
        &mut self,
        context: &mut EvaluationPollContext,
        mut pathing: StatePathWork<S>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> StatePathStep<S> {
        match &mut pathing.operation {
            StatePathOperation::Keys { machine, after } => {
                let poll = context.evaluate(&self.eval_context, |evaluator| {
                    machine.poll(context, evaluator, &self.eval_context, step_budget)
                });
                match poll {
                    eval::ConversionPoll::Ready(keys) => {
                        let after = after
                            .take()
                            .expect("key path continuation must remain owned");
                        if let StatePathAfterKeys::Store(operation) = after {
                            if keys.is_empty()
                                && matches!(
                                    operation,
                                    StorePathOperation::HeapDel | StorePathOperation::VolumeDel(_)
                                )
                            {
                                return StatePathStep::Failed(
                                    pathing,
                                    TaskHalt::new("`.del` needs a non-empty path"),
                                );
                            }
                            return self.store_path_step(
                                context,
                                operation,
                                keys,
                                pathing.branch,
                                pathing.scope_depth,
                            );
                        }
                        pathing.operation = match after {
                            StatePathAfterKeys::Get { state } => StatePathOperation::Get(
                                ValuePathMachine::new(state, Arc::from(keys)),
                            ),
                            StatePathAfterKeys::Set { state, value } => {
                                let path: Arc<[Key]> = Arc::from(keys);
                                let focus = if path.is_empty() {
                                    value.clone()
                                } else {
                                    state
                                };
                                StatePathOperation::SetBase {
                                    computation: WhnfComputation::from_root(focus),
                                    path,
                                    value,
                                }
                            }
                            StatePathAfterKeys::Store(_) => {
                                unreachable!("store paths returned above")
                            }
                        };
                        StatePathStep::Continue(pathing)
                    }
                    eval::ConversionPoll::Pending(dependency) => {
                        StatePathStep::Blocked(pathing, dependency)
                    }
                    eval::ConversionPoll::BudgetExhausted => {
                        StatePathStep::BudgetExhausted(pathing)
                    }
                    eval::ConversionPoll::Failed(failure) => {
                        StatePathStep::Failed(pathing, TaskHalt::rooted_failure(failure))
                    }
                }
            }
            StatePathOperation::Get(machine) => {
                match machine.poll(context, &self.eval_context, step_budget) {
                    ValuePathPoll::Ready(value) => StatePathStep::Complete(
                        MachineWork::deliver_root(value, pathing.branch, pathing.scope_depth),
                    ),
                    ValuePathPoll::Pending(dependency) => {
                        StatePathStep::Blocked(pathing, dependency)
                    }
                    ValuePathPoll::Continue => StatePathStep::Continue(pathing),
                    ValuePathPoll::BudgetExhausted => StatePathStep::BudgetExhausted(pathing),
                    ValuePathPoll::Failed(error) => StatePathStep::Failed(pathing, error),
                }
            }
            StatePathOperation::SetBase {
                computation,
                path,
                value,
            } => match poll_whnf_computation(computation, context, &self.eval_context, step_budget)
            {
                WhnfOwnerPoll::Ready(base) => {
                    let is_dict = context.evaluate(&self.eval_context, |evaluator| {
                        evaluator.project_root(&base, |_, base| matches!(base, Value::Dict(_)))
                    });
                    if !is_dict {
                        return StatePathStep::Failed(
                            pathing,
                            TaskHalt::new("reflection user state must be a dictionary"),
                        );
                    }
                    if path.is_empty() {
                        pathing.branch.state = base;
                        return StatePathStep::Complete(context.with_runtime_access(|access| {
                            MachineWork::deliver_unit(&access, pathing.branch, pathing.scope_depth)
                        }));
                    }
                    let update = context.evaluate(&self.eval_context, |evaluator| {
                        evaluator.with_value_access(|access| {
                            let path = Value::List(List::from_values(
                                path.iter()
                                    .map(|key| key.to_value_in(access.values()))
                                    .collect(),
                            ));
                            let value = access.clone_root(value);
                            let base = access.clone_root(&base);
                            let update = Value::builtin_call_in(
                                access.values(),
                                Builtin::DictInsert,
                                vec![path, value, base],
                            );
                            access.root_value(update)
                        })
                    });
                    pathing.operation =
                        StatePathOperation::SetUpdate(WhnfComputation::from_root(update));
                    StatePathStep::Continue(pathing)
                }
                WhnfOwnerPoll::Pending(dependency) => StatePathStep::Blocked(pathing, dependency),
                WhnfOwnerPoll::BudgetExhausted => StatePathStep::BudgetExhausted(pathing),
                WhnfOwnerPoll::Failed(failure) => {
                    StatePathStep::Failed(pathing, TaskHalt::rooted_failure(failure))
                }
            },
            StatePathOperation::SetUpdate(computation) => {
                match poll_whnf_computation(computation, context, &self.eval_context, step_budget) {
                    WhnfOwnerPoll::Ready(state) => {
                        pathing.branch.state = state;
                        StatePathStep::Complete(context.with_runtime_access(|access| {
                            MachineWork::deliver_unit(&access, pathing.branch, pathing.scope_depth)
                        }))
                    }
                    WhnfOwnerPoll::Pending(dependency) => {
                        StatePathStep::Blocked(pathing, dependency)
                    }
                    WhnfOwnerPoll::BudgetExhausted => StatePathStep::BudgetExhausted(pathing),
                    WhnfOwnerPoll::Failed(failure) => {
                        StatePathStep::Failed(pathing, TaskHalt::rooted_failure(failure))
                    }
                }
            }
            StatePathOperation::Poisoned => {
                unreachable!("failed state-path work cannot be resumed before error handling")
            }
        }
    }

    fn store_path_step(
        &mut self,
        context: &mut EvaluationPollContext,
        operation: StorePathOperation,
        path: Vec<Key>,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> StatePathStep<S> {
        let result = match operation {
            StorePathOperation::HeapGet => {
                let checkpoint = branch.retry_candidate();
                let heap = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let observed = transaction.store.observe_read(&path);
                    let heap =
                        context.with_runtime_access(|access| transaction.store.view_in(&access));
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    heap
                } else {
                    context.suspend(|| self.host.snapshot().store().root().clone())
                };
                let value = context.with_runtime_access(|access| {
                    lazy_value_path_root(&access, &heap.clone().into_runtime_root(), &path)
                });
                return StatePathStep::Complete(MachineWork::deliver_root(
                    value,
                    branch,
                    scope_depth,
                ));
            }
            StorePathOperation::HeapSet(value) => {
                if let Some(transaction) = branch.transaction.as_mut() {
                    context.with_runtime_access(|access| {
                        transaction.store.write_in(
                            &access,
                            path,
                            PublicValue::from_runtime_root(value),
                        )
                    });
                    return StatePathStep::Complete(context.with_runtime_access(|access| {
                        MachineWork::deliver_unit(&access, branch, scope_depth)
                    }));
                }
                // One suspension covers the snapshot, the edit and the
                // commit.
                context.suspend(|| {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.write(path, PublicValue::from_runtime_root(value));
                    self.host.commit(TaskCommit::new(
                        store,
                        snapshot.extra().clone(),
                        S::Journal::default(),
                    ))
                })
            }
            StorePathOperation::HeapRewrite(updater) => {
                if let Some(transaction) = branch.transaction.as_mut() {
                    context.with_runtime_access(|access| {
                        transaction.store.rewrite_in(
                            &access,
                            path,
                            PublicValue::from_runtime_root(updater),
                        )
                    });
                    return StatePathStep::Complete(context.with_runtime_access(|access| {
                        MachineWork::deliver_unit(&access, branch, scope_depth)
                    }));
                }
                // One suspension covers the snapshot, the edit and the
                // commit.
                context.suspend(|| {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.rewrite(path, PublicValue::from_runtime_root(updater));
                    self.host.commit(TaskCommit::new(
                        store,
                        snapshot.extra().clone(),
                        S::Journal::default(),
                    ))
                })
            }
            StorePathOperation::HeapTryGet => {
                let checkpoint = branch.retry_candidate();
                let heap = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let observed = transaction.store.observe_read(&path);
                    let heap =
                        context.with_runtime_access(|access| transaction.store.view_in(&access));
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    heap
                } else {
                    context.suspend(|| self.host.snapshot().store().root().clone())
                };
                return StatePathStep::Complete(context.with_runtime_access(|access| {
                    let heap = heap.into_runtime_root().clone_core_with(&access);
                    let path = key_path_in(&access, &path);
                    try_get_work(&access, &heap, path, branch, scope_depth)
                }));
            }
            StorePathOperation::HeapDel => {
                if let Some(transaction) = branch.transaction.as_mut() {
                    context
                        .with_runtime_access(|access| transaction.store.remove_in(&access, path));
                    return StatePathStep::Complete(context.with_runtime_access(|access| {
                        MachineWork::deliver_unit(&access, branch, scope_depth)
                    }));
                }
                // One suspension covers the snapshot, the edit and the
                // commit.
                context.suspend(|| {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.remove(path);
                    self.host.commit(TaskCommit::new(
                        store,
                        snapshot.extra().clone(),
                        S::Journal::default(),
                    ))
                })
            }
            StorePathOperation::VolumeTryGet(target) => {
                let checkpoint = branch.retry_candidate();
                let read = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let (observed, read) = context.with_runtime_access(|access| {
                        let observed = target.observe_read(&mut transaction.store, &access, &path);
                        let read =
                            target
                                .read_in(&mut transaction.store, &access)
                                .map(|contents| {
                                    access.root_runtime_value(contents_at_path(
                                        &access,
                                        contents,
                                        &[],
                                    ))
                                });
                        (observed, read)
                    });
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    read
                } else {
                    let snapshot = context.suspend(|| self.host.snapshot().store().clone());
                    context.with_runtime_access(|access| {
                        target.read_snapshot_in(&snapshot, &access).map(|contents| {
                            access.root_runtime_value(contents_at_path(&access, contents, &[]))
                        })
                    })
                };
                return StatePathStep::Complete(context.with_runtime_access(|access| match read {
                    Some(contents) => {
                        let contents = contents.clone_core_with(&access);
                        let path = key_path_in(&access, &path);
                        try_get_work(&access, &contents, path, branch, scope_depth)
                    }
                    None => {
                        let volume = target.id_in(&access);
                        let error = access.root_runtime_value(Value::Lazy(LazyValue::error_in(
                            &access,
                            format!("reflection volume {} has been revoked", volume.get()),
                        )));
                        MachineWork::deliver_root(error, branch, scope_depth)
                    }
                }));
            }
            StorePathOperation::VolumeDel(target) => {
                if let Some(transaction) = branch.transaction.as_mut() {
                    context.with_runtime_access(|access| {
                        target.remove_in(&mut transaction.store, &access, path)
                    });
                    return StatePathStep::Complete(context.with_runtime_access(|access| {
                        MachineWork::deliver_unit(&access, branch, scope_depth)
                    }));
                }
                // One suspension covers the snapshot, the edit and the
                // commit, while the target keeps the volume.
                context.suspend(|| {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.with_access(|store, access| target.remove_in(store, access, path));
                    self.host.commit(TaskCommit::new(
                        store,
                        snapshot.extra().clone(),
                        S::Journal::default(),
                    ))
                })
            }
            StorePathOperation::VolumeGet(target) => {
                let checkpoint = branch.retry_candidate();
                // A read roots only its result; within a transaction, the
                // journal keeps the volume it read (`api-volume-cells`).
                let read = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let (observed, read) = context.with_runtime_access(|access| {
                        let observed = target.observe_read(&mut transaction.store, &access, &path);
                        let read =
                            target
                                .read_in(&mut transaction.store, &access)
                                .map(|contents| {
                                    access.root_runtime_value(contents_at_path(
                                        &access, contents, &path,
                                    ))
                                });
                        (observed, read)
                    });
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    read
                } else {
                    let snapshot = context.suspend(|| self.host.snapshot().store().clone());
                    context.with_runtime_access(|access| {
                        target.read_snapshot_in(&snapshot, &access).map(|contents| {
                            access.root_runtime_value(contents_at_path(&access, contents, &path))
                        })
                    })
                };
                let value = match read {
                    Some(value) => value,
                    None => context.with_runtime_access(|access| {
                        let volume = target.id_in(&access);
                        access.root_runtime_value(Value::Lazy(LazyValue::error_in(
                            &access,
                            format!("reflection volume {} has been revoked", volume.get()),
                        )))
                    }),
                };
                return StatePathStep::Complete(MachineWork::deliver_root(
                    value,
                    branch,
                    scope_depth,
                ));
            }
            StorePathOperation::VolumeSet(target, value) => {
                let value = PublicValue::from_runtime_root(value);
                if let Some(transaction) = branch.transaction.as_mut() {
                    context.with_runtime_access(|access| {
                        target.write_in(&mut transaction.store, &access, path, value)
                    });
                    return StatePathStep::Complete(context.with_runtime_access(|access| {
                        MachineWork::deliver_unit(&access, branch, scope_depth)
                    }));
                }
                // One suspension covers the snapshot, the edit and the
                // commit, while the target keeps the volume.
                context.suspend(|| {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.with_access(|store, access| target.write_in(store, access, path, value));
                    self.host.commit(TaskCommit::new(
                        store,
                        snapshot.extra().clone(),
                        S::Journal::default(),
                    ))
                })
            }
            StorePathOperation::VolumeRewrite(target, updater) => {
                let updater = PublicValue::from_runtime_root(updater);
                if let Some(transaction) = branch.transaction.as_mut() {
                    context.with_runtime_access(|access| {
                        target.rewrite_in(&mut transaction.store, &access, path, updater)
                    });
                    return StatePathStep::Complete(context.with_runtime_access(|access| {
                        MachineWork::deliver_unit(&access, branch, scope_depth)
                    }));
                }
                // One suspension covers the snapshot, the edit and the
                // commit, while the target keeps the volume.
                context.suspend(|| {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.with_access(|store, access| {
                        target.rewrite_in(store, access, path, updater)
                    });
                    self.host.commit(TaskCommit::new(
                        store,
                        snapshot.extra().clone(),
                        S::Journal::default(),
                    ))
                })
            }
        };
        StatePathStep::Complete(match result {
            CommitResult::Committed => {
                branch.retry = None;
                context.with_runtime_access(|access| {
                    MachineWork::deliver_unit(&access, branch, scope_depth)
                })
            }
            CommitResult::Conflict => MachineWork::Drive {
                branch,
                scope_depth,
            },
            CommitResult::MissingVolume(volume) => {
                return StatePathStep::Failed(
                    StatePathWork::poisoned(branch, scope_depth),
                    missing_volume_error(volume),
                );
            }
            CommitResult::Closed => MachineWork::Outcome {
                outcome: BranchOutcome::Cancelled,
                scope_depth,
            },
        })
    }

    fn complete_decode_phase(
        &mut self,
        context: &EvaluationPollContext,
        decoding: EffectDecodeWork<S>,
        purpose: EffectDecodePurpose,
        value: RuntimeValueRoot,
    ) -> EffectDecodeStep<S> {
        let EffectDecodeWork {
            branch,
            scope_depth,
            ..
        } = decoding;
        match purpose {
            EffectDecodePurpose::EffectObject => {
                let function = context.evaluate(&self.eval_context, |evaluator| {
                    evaluator.project_root(&value, |access, effect| {
                        let Value::Dict(effect) = effect else {
                            return Err(TaskHalt::new(format!(
                                "reflection task requires an effect object, got {:?}",
                                access.values().diagnostic_debug(&effect)
                            )));
                        };
                        effect
                            .get(&*keys::EFF)
                            .map(|function| access.values().duplicate_value(function))
                            .map(|function| access.root_value(function))
                            .ok_or_else(|| TaskHalt::new("reflection effect has no `eff` member"))
                    })
                });
                match function {
                    Ok(function) => EffectDecodeStep::Continue(EffectDecodeWork::from_root(
                        function,
                        EffectDecodePurpose::Function,
                        branch,
                        scope_depth,
                    )),
                    Err(error) => EffectDecodeStep::Failed(
                        EffectDecodeWork::from_root(
                            value,
                            EffectDecodePurpose::EffectObject,
                            branch,
                            scope_depth,
                        ),
                        error,
                    ),
                }
            }
            EffectDecodePurpose::Function => {
                #[cfg(test)]
                if let Some(probe) = &self.phase_probe {
                    probe.record_application_start();
                }
                #[cfg(test)]
                self.eval_context.arm_deferred_pump_pause();
                EffectDecodeStep::Continue(EffectDecodeWork::application(
                    context,
                    &self.eval_context,
                    value,
                    vec![self.api.clone()],
                    EffectDecodePurpose::RequestApplication,
                    branch,
                    scope_depth,
                ))
            }
            EffectDecodePurpose::RequestApplication => {
                EffectDecodeStep::Continue(EffectDecodeWork::request(value, branch, scope_depth))
            }
            EffectDecodePurpose::AppliedEffect => {
                let mut branch = branch;
                branch.set_effect_root(value);
                EffectDecodeStep::Complete(MachineWork::Drive {
                    branch,
                    scope_depth,
                })
            }
        }
    }

    fn step(
        &mut self,
        context: &mut EvaluationPollContext,
        work: MachineWork<S>,
    ) -> Result<MachineStep<S>, TaskHalt> {
        match work {
            MachineWork::Drive {
                branch,
                scope_depth,
            } => self.drive_step(context, branch, scope_depth),
            MachineWork::Deliver {
                value,
                branch,
                scope_depth,
            } => self.deliver_step(context, value, branch, scope_depth),
            MachineWork::Apply {
                function,
                arguments,
                branch,
                scope_depth,
            } => {
                #[cfg(test)]
                self.record_phase(EffectMachinePhase::ContinuationDelivered);
                Ok(MachineStep::Decode(EffectDecodeWork::application(
                    context,
                    &self.eval_context,
                    function,
                    arguments,
                    EffectDecodePurpose::AppliedEffect,
                    branch,
                    scope_depth,
                )))
            }
            MachineWork::Interpret {
                request,
                branch,
                scope_depth,
            } => self.interpret_decoded_drive(context, request, branch, scope_depth),
            MachineWork::Outcome {
                outcome,
                scope_depth,
            } => self.handle_outcome(context, outcome, scope_depth),
        }
    }

    fn drive_step(
        &mut self,
        _context: &EvaluationPollContext,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        Ok(MachineStep::Decode(EffectDecodeWork::new(
            branch,
            scope_depth,
        )))
    }

    fn interpret_decoded_drive(
        &mut self,
        context: &mut EvaluationPollContext,
        request: Request<S::Request>,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        if self.fusion_enabled() {
            match request {
                Request::Seq(operation, continuation) => {
                    self.record_fused_request();
                    branch
                        .control
                        .sequence
                        .push(Continuation::Glam(continuation));
                    branch.set_effect_root(operation);
                    return Ok(MachineStep::Decode(EffectDecodeWork::new(
                        branch,
                        scope_depth,
                    )));
                }
                Request::Return(value) => {
                    return self.finish_fused_delivery(context, branch, value, scope_depth);
                }
                Request::Get(path) => {
                    #[cfg(test)]
                    self.record_phase(EffectMachinePhase::InterpreterEntered);
                    return Ok(MachineStep::StatePath(Box::new(StatePathWork::get(
                        path,
                        branch,
                        scope_depth,
                    ))));
                }
                Request::Set(path, value) => {
                    #[cfg(test)]
                    self.record_phase(EffectMachinePhase::InterpreterEntered);
                    return Ok(MachineStep::StatePath(Box::new(StatePathWork::set(
                        path,
                        value,
                        branch,
                        scope_depth,
                    ))));
                }
                request => {
                    return self.interpret_request(context, request, branch, scope_depth);
                }
            }
        }
        self.interpret_request(context, request, branch, scope_depth)
    }

    fn finish_fused_delivery(
        &mut self,
        context: &mut EvaluationPollContext,
        branch: Branch<S>,
        value: RuntimeValueRoot,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        if let Some(Continuation::Glam(function)) = branch.control.sequence.last().cloned() {
            return Ok(MachineStep::Demand(ScalarDemandWork::new(
                function,
                ScalarDemandPurpose::ApplyContinuation {
                    argument: value,
                    fused: true,
                },
                branch,
                scope_depth,
            )));
        }
        self.interpret_request(context, Request::Return(value), branch, scope_depth)
    }

    /// The store as the branch sees it: the transaction's snapshot within a
    /// cut, or the host's.
    fn store_snapshot(
        &self,
        context: &mut EvaluationPollContext,
        branch: &Branch<S>,
    ) -> StoreSnapshot {
        match &branch.transaction {
            Some(transaction) => transaction.snapshot.store().clone(),
            None => context.suspend(|| self.host.snapshot().store().clone()),
        }
    }

    /// The store's promises.
    fn promises(&self, context: &mut EvaluationPollContext, branch: &Branch<S>) -> Arc<Promises> {
        self.store_snapshot(context, branch).promises().clone()
    }

    /// Resolves `promise` (`promise-resolution-commits-with-its-cut`):
    /// within a cut, as part of its transaction, when the outermost cut
    /// commits; outside one, in a commit of its own. A promise already
    /// resolved, or resolved within this cut already, is an error, and so
    /// is one that another resolution beats to the commit, found running
    /// again.
    fn resolve_promise(
        &self,
        context: &mut EvaluationPollContext,
        branch: &mut Branch<S>,
        resolver: ManagedResolverRoot,
        resolution: Resolution,
    ) -> Result<(), TaskHalt> {
        if let Some(transaction) = branch.transaction.as_mut() {
            return transaction
                .store
                .resolve(&resolver, resolution)
                .map_err(|error| TaskHalt::new(error.as_ref()));
        }
        // One suspension covers each attempt's snapshot, staging and commit.
        context.suspend(|| {
            loop {
                let snapshot = self.host.snapshot();
                let mut store = StoreJournal::new(snapshot.store().clone());
                store
                    .resolve(&resolver, resolution.clone())
                    .map_err(|error| TaskHalt::new(error.as_ref()))?;
                match self.host.commit(TaskCommit::new(
                    store,
                    snapshot.extra().clone(),
                    S::Journal::default(),
                )) {
                    CommitResult::Conflict => {}
                    CommitResult::Committed | CommitResult::Closed => return Ok(()),
                    CommitResult::MissingVolume(volume) => {
                        return Err(missing_volume_error(volume));
                    }
                }
            }
        })
    }

    fn interpret_request(
        &mut self,
        context: &mut EvaluationPollContext,
        request: Request<S::Request>,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        #[cfg(test)]
        self.record_phase(EffectMachinePhase::InterpreterEntered);
        let work = match request {
            Request::Return(value) => MachineWork::deliver_root(value, branch, scope_depth),
            Request::Seq(operation, continuation) => {
                branch
                    .control
                    .sequence
                    .push(Continuation::Glam(continuation));
                branch.set_effect_root(operation);
                MachineWork::Drive {
                    branch,
                    scope_depth,
                }
            }
            // A choice's first operand is atomic: `.alt a b` means
            // `.alt (.cut a) b` (`alt-cuts-its-first-operand`). Isolated
            // search keeps every branch instead.
            Request::Alt(left, right) => {
                let retains_all = self.search.retains_all();
                if (scope_depth > 0 || retains_all) && !branch.active_fixes.is_empty() {
                    let inherited_restarts = branch.fix_restarts.clone();
                    let active = branch
                        .active_fixes
                        .first_mut()
                        .expect("checked nonempty fixpoint stack");
                    let choice = if let Some(choice) = active.choices.get(active.next_choice) {
                        active.next_choice += 1;
                        *choice
                    } else {
                        let root = active.root.clone();
                        let mut right_choices = active.choices.clone();
                        right_choices.push(FixChoice::Right);
                        active.choices.push(FixChoice::Left);
                        active.next_choice += 1;
                        branch.fix_restarts.push(FixRestart {
                            root,
                            choices: right_choices,
                            inherited_restarts,
                        });
                        self.hosted_set_aside();
                        FixChoice::Left
                    };
                    match choice {
                        FixChoice::Left if !retains_all => {
                            return Ok(MachineStep::Continue(self.enter_cut(
                                context,
                                left,
                                branch,
                                scope_depth,
                            )));
                        }
                        FixChoice::Left => branch.set_effect_root(left),
                        FixChoice::Right => branch.set_effect_root(right),
                    }
                    MachineWork::Drive {
                        branch,
                        scope_depth,
                    }
                } else if retains_all {
                    MachineWork::Outcome {
                        outcome: BranchOutcome::Fork(
                            Box::new(branch.with_effect_root(left)),
                            Box::new(branch.with_effect_root(right)),
                        ),
                        scope_depth,
                    }
                } else if scope_depth > 0 {
                    // Within a cut, the second operand stays open to the
                    // cut's later failures.
                    let alternative = branch.with_effect_root(right);
                    let frame = self
                        .execution
                        .cuts
                        .last_mut()
                        .expect("a branch within a cut runs in its frame");
                    debug_assert_eq!(frame.scope_depth, scope_depth);
                    frame.alternatives.push(alternative);
                    self.hosted_set_aside();
                    return Ok(MachineStep::Continue(self.enter_cut(
                        context,
                        left,
                        branch,
                        scope_depth,
                    )));
                } else {
                    // Outside every cut, nothing reconsiders the choice once
                    // its first operand succeeds: the second is a fallback
                    // for that operand's failure alone.
                    let fallback = branch.with_effect_root(right);
                    let work = self.enter_cut(context, left, branch, scope_depth);
                    self.execution
                        .cuts
                        .last_mut()
                        .expect("the cut was just entered")
                        .fallback = Some(fallback);
                    return Ok(MachineStep::Continue(work));
                }
            }
            Request::Fail => MachineWork::Outcome {
                outcome: branch.into_failure(),
                scope_depth,
            },
            Request::Cut(operation) => {
                return Ok(MachineStep::Continue(self.enter_cut(
                    context,
                    operation,
                    branch,
                    scope_depth,
                )));
            }
            Request::Try(_) | Request::Eval(_) if self.search.retains_all() => {
                return Err(TaskHalt::new(
                    "`.try` and `.eval` are not available in an isolated search",
                ));
            }
            // `.eval V` is `.try (seq V (.r V))` (maintainer, 2026-10-23):
            // its value's weak-head normal form as `ok`, an evaluation error
            // as `err`, or a promise no one has resolved as `wait`.
            Request::Eval(value) => {
                let operation = context.with_runtime_access(|access| {
                    let value = value.clone_core_with(&access);
                    branch.root_value(&access, evaluation_effect_in(&access, value))
                });
                let work = self.enter_cut(context, operation, branch, scope_depth);
                self.execution
                    .cuts
                    .last_mut()
                    .expect("the cut was just entered")
                    .trying = true;
                return Ok(MachineStep::Continue(work));
            }
            Request::Try(operation) => {
                let work = self.enter_cut(context, operation, branch, scope_depth);
                self.execution
                    .cuts
                    .last_mut()
                    .expect("the cut was just entered")
                    .trying = true;
                return Ok(MachineStep::Continue(work));
            }
            Request::Get(path) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::get(
                    path,
                    branch,
                    scope_depth,
                ))));
            }
            Request::Set(path, value) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::set(
                    path,
                    value,
                    branch,
                    scope_depth,
                ))));
            }
            Request::HeapGet(path) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::heap_get(
                    path,
                    branch,
                    scope_depth,
                ))));
            }
            // The user's state is the task's own: no read to observe.
            Request::TryGet(path) => context.with_runtime_access(|access| {
                let state = branch.state.clone_core_with(&access);
                let path = path.clone_core_with(&access);
                try_get_work(&access, &state, path, branch, scope_depth)
            }),
            Request::Del(path) => {
                let update = context.with_runtime_access(|access| {
                    let path = path.clone_core_with(&access);
                    let state = branch.state.clone_core_with(&access);
                    branch.root_value(
                        &access,
                        Value::builtin_call_in(&access, Builtin::DictRemove, vec![path, state]),
                    )
                });
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::del(
                    update,
                    branch,
                    scope_depth,
                ))));
            }
            Request::HeapTryGet(path) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::store(
                    path,
                    StorePathOperation::HeapTryGet,
                    branch,
                    scope_depth,
                ))));
            }
            Request::HeapDel(path) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::store(
                    path,
                    StorePathOperation::HeapDel,
                    branch,
                    scope_depth,
                ))));
            }
            Request::VolumeTryGet(volume, path) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::store(
                    path,
                    StorePathOperation::VolumeTryGet(volume),
                    branch,
                    scope_depth,
                ))));
            }
            Request::VolumeDel(volume, path) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::store(
                    path,
                    StorePathOperation::VolumeDel(volume),
                    branch,
                    scope_depth,
                ))));
            }
            Request::HeapSet(path, value) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::heap_set(
                    path,
                    value,
                    branch,
                    scope_depth,
                ))));
            }
            Request::HeapRewrite(path, updater) => {
                return Ok(MachineStep::StatePath(Box::new(
                    StatePathWork::heap_rewrite(path, updater, branch, scope_depth),
                )));
            }
            Request::VolumeGet(volume, path) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::volume_get(
                    volume,
                    path,
                    branch,
                    scope_depth,
                ))));
            }
            Request::VolumeSet(volume, path, value) => {
                return Ok(MachineStep::StatePath(Box::new(StatePathWork::volume_set(
                    volume,
                    path,
                    value,
                    branch,
                    scope_depth,
                ))));
            }
            Request::VolumeRewrite(volume, path, updater) => {
                return Ok(MachineStep::StatePath(Box::new(
                    StatePathWork::volume_rewrite(volume, path, updater, branch, scope_depth),
                )));
            }
            Request::Reset(key, operation) => {
                let stack = context.evaluate(&self.eval_context, |evaluator| {
                    reset_stack_root_in(evaluator, &branch.state, &self.tags.continuation_state)
                })?;
                return Ok(MachineStep::Control(Box::new(ControlWork::reset(
                    key,
                    stack,
                    operation,
                    branch,
                    scope_depth,
                ))));
            }
            Request::Shift(key, function) => {
                let stack = context.evaluate(&self.eval_context, |evaluator| {
                    reset_stack_root_in(evaluator, &branch.state, &self.tags.continuation_state)
                })?;
                return Ok(MachineStep::Control(Box::new(ControlWork::shift(
                    key,
                    stack,
                    function,
                    branch,
                    scope_depth,
                ))));
            }
            Request::Resume(task_id, id, value) => {
                if task_id != self.id {
                    return Err(TaskHalt::new(
                        "captured continuation belongs to another reflection task",
                    ));
                }
                let captured = self
                    .continuations
                    .get(&ContinuationKey(id))
                    .cloned()
                    .ok_or_else(|| TaskHalt::new("unknown reflection continuation"))?;
                let stack = context.evaluate(&self.eval_context, |evaluator| {
                    reset_stack_root_in(evaluator, &branch.state, &self.tags.continuation_state)
                })?;
                return Ok(MachineStep::Control(Box::new(
                    ControlWork::install_captured(stack, captured, value, branch, scope_depth),
                )));
            }
            Request::ExitSuccess => {
                return Ok(MachineStep::Exit(ExitIntent::Success));
            }
            Request::ExitError(message) => {
                return Ok(MachineStep::Demand(ScalarDemandWork::new(
                    message,
                    ScalarDemandPurpose::ExitError,
                    branch,
                    scope_depth,
                )));
            }
            Request::Fix(function) => {
                // A restart resumes the fixpoint's entry.
                self.hosted_set_aside();
                let root = Arc::new(FixRoot {
                    function,
                    entry: branch,
                    scope_depth,
                });
                let stack = context.evaluate(&self.eval_context, |evaluator| {
                    reset_stack_root_in(evaluator, &root.entry.state, &self.tags.continuation_state)
                })?;
                return Ok(MachineStep::Control(Box::new(ControlWork::start_fixpoint(
                    stack,
                    root,
                    Vec::new(),
                ))));
            }
            Request::VolumeNew(initial) => {
                let initial = PublicValue::from_runtime_root(initial);
                let handle = if let Some(transaction) = branch.transaction.as_mut() {
                    context
                        .with_runtime_access(|access| {
                            let volume = transaction.store.create_volume_in(&access, initial)?;
                            Ok::<_, Arc<str>>(access.root_runtime_value(volume.handle_in(&access)))
                        })
                        .map_err(|error| TaskHalt::new(error.as_ref()))?
                } else {
                    // One suspension covers the snapshot, the allocation and
                    // the commit.
                    let (result, handle) = context.suspend(|| {
                        let snapshot = self.host.snapshot();
                        let mut store = StoreJournal::new(snapshot.store().clone());
                        let handle = store
                            .create_volume(initial)
                            .map_err(|error| TaskHalt::new(error.as_ref()))?
                            .into_runtime_root();
                        let result = self.host.commit(TaskCommit::new(
                            store,
                            snapshot.extra().clone(),
                            S::Journal::default(),
                        ));
                        Ok::<_, TaskHalt>((result, handle))
                    })?;
                    match result {
                        CommitResult::Committed => handle,
                        CommitResult::Conflict => {
                            return Ok(MachineStep::Continue(MachineWork::Drive {
                                branch,
                                scope_depth,
                            }));
                        }
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => {
                            return Ok(MachineStep::Continue(MachineWork::Outcome {
                                outcome: BranchOutcome::Cancelled,
                                scope_depth,
                            }));
                        }
                    }
                };
                MachineWork::deliver_root(handle, branch, scope_depth)
            }
            Request::HeapVolume => {
                let store = self.store_snapshot(context, &branch);
                let handle = context.with_runtime_access(|access| {
                    access.root_runtime_value(store.heap_handle(&access))
                });
                MachineWork::deliver_root(handle, branch, scope_depth)
            }
            Request::PromiseNew => {
                let promises = self.promises(context, &branch);
                let pair = context.with_runtime_access(|access| {
                    let (_, promised, resolver) = promises.allocate(&access);
                    access.root_runtime_value(Value::Dict(
                        Dict::new_sync()
                            .insert(Key::atom_from_text("promise"), promised)
                            .insert(Key::atom_from_text("resolver"), resolver),
                    ))
                });
                MachineWork::deliver_root(pair, branch, scope_depth)
            }
            Request::PromiseStatus(resolver) => {
                let checkpoint = branch.retry_candidate();
                let promise = resolver.id();
                // The cut sees its own resolution, as its own writes.
                let status = branch
                    .transaction
                    .as_ref()
                    .and_then(|transaction| transaction.store.staged_status(promise))
                    .unwrap_or_else(|| resolver.status());
                // Within a cut the read joins the transaction, which the
                // promise's resolution then conflicts with.
                if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    if transaction.store.observe_promise_read(promise) {
                        branch.observe(checkpoint, generation);
                    }
                }
                let status = context.with_runtime_access(|access| {
                    access.root_runtime_value(Value::Atom(Atom::from_key(&Key::binary_from_text(
                        status.name(),
                    ))))
                });
                MachineWork::deliver_root(status, branch, scope_depth)
            }
            Request::PromiseFulfill(promise, value) => {
                self.resolve_promise(context, &mut branch, promise, Resolution::Fulfill(value))?;
                context.with_runtime_access(|access| {
                    MachineWork::deliver_unit(&access, branch, scope_depth)
                })
            }
            Request::PromiseBreak(promise, message) => {
                self.resolve_promise(context, &mut branch, promise, Resolution::Break(message))?;
                context.with_runtime_access(|access| {
                    MachineWork::deliver_unit(&access, branch, scope_depth)
                })
            }
            Request::Specialized(request, arguments) => {
                let request = SpecializationRequest::Unstarted {
                    request,
                    arguments: arguments
                        .into_iter()
                        .map(PublicValue::from_runtime_root)
                        .collect(),
                };
                return Ok(MachineStep::Specialize(Box::new(SpecializationWork::new(
                    request,
                    branch,
                    scope_depth,
                ))));
            }
        };
        Ok(MachineStep::Continue(work))
    }

    fn complete_specialization_request(
        &self,
        context: &EvaluationPollContext,
        result: RequestResult,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> MachineWork<S> {
        match result {
            RequestResult::Return(value) => {
                MachineWork::deliver_root(value.into_runtime_root(), branch, scope_depth)
            }
            RequestResult::Alternatives(values) => match values.as_slice() {
                [] => MachineWork::Outcome {
                    outcome: branch.into_failure(),
                    scope_depth,
                },
                [value] => MachineWork::deliver_root(
                    value.clone().into_runtime_root(),
                    branch,
                    scope_depth,
                ),
                _ => MachineWork::Drive {
                    branch: branch.with_effect_root(context.with_runtime_access(|access| {
                        alternative_returns_root(&access, &self.tags, &values)
                    })),
                    scope_depth,
                },
            },
            RequestResult::Scoped { operation, close } => {
                branch
                    .control
                    .sequence
                    .push(Continuation::CloseScope(close.into_runtime_root()));
                MachineWork::Drive {
                    branch: branch.with_effect_root(operation.into_runtime_root()),
                    scope_depth,
                }
            }
            RequestResult::ReturnUnit => context.with_runtime_access(|access| {
                MachineWork::deliver_unit(&access, branch, scope_depth)
            }),
            RequestResult::Fail => MachineWork::Outcome {
                outcome: branch.into_failure(),
                scope_depth,
            },
            RequestResult::Cancelled => MachineWork::Outcome {
                outcome: BranchOutcome::Cancelled,
                scope_depth,
            },
        }
    }

    fn validate_specialization_result(&self, result: &RequestResult) -> Result<(), TaskHalt> {
        let values = Values::from_core_factory(self.eval_context.values().clone());
        let validate = |value: &PublicValue| {
            values
                .require(value)
                .map_err(|error| TaskHalt::new(error.to_string()))
        };
        match result {
            RequestResult::Return(value) => validate(value),
            RequestResult::Alternatives(values) => values.iter().try_for_each(validate),
            RequestResult::Scoped { operation, close } => {
                validate(operation)?;
                validate(close)
            }
            RequestResult::ReturnUnit | RequestResult::Fail | RequestResult::Cancelled => Ok(()),
        }
    }

    fn deliver_step(
        &mut self,
        context: &EvaluationPollContext,
        value: RuntimeValueRoot,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        if let Some(continuation) = branch.control.sequence.last().cloned() {
            return match continuation {
                Continuation::Glam(function) => Ok(MachineStep::Demand(ScalarDemandWork::new(
                    function,
                    ScalarDemandPurpose::ApplyContinuation {
                        argument: value,
                        fused: false,
                    },
                    branch,
                    scope_depth,
                ))),
                Continuation::RequireUnit => Ok(MachineStep::Demand(ScalarDemandWork::new(
                    value,
                    ScalarDemandPurpose::RequireUnit,
                    branch,
                    scope_depth,
                ))),
                Continuation::AssertUnit(diagnostic_context) => {
                    let assertion = context.evaluate(&self.eval_context, |evaluator| {
                        evaluator.with_value_access(|access| {
                            let diagnostic_context = access.clone_root(&diagnostic_context);
                            let value = access.clone_root(&value);
                            let assertion = Value::builtin_call_in(
                                access.values(),
                                Builtin::AssertUnit,
                                vec![diagnostic_context, value, access.values().unit()],
                            );
                            access.root_value(assertion)
                        })
                    });
                    Ok(MachineStep::Demand(ScalarDemandWork::new(
                        assertion,
                        ScalarDemandPurpose::AssertUnit,
                        branch,
                        scope_depth,
                    )))
                }
                Continuation::Fix(handle) => {
                    let active = branch.active_fixes.last().ok_or_else(|| {
                        TaskHalt::new("reflection fixpoint lost its active branch")
                    })?;
                    if active.handle.id() != handle.id() {
                        return Err(TaskHalt::new(
                            "reflection fixpoint control became unbalanced",
                        ));
                    }
                    if active.next_choice != active.choices.len() {
                        return Err(TaskHalt::new("reflection fixpoint choice replay diverged"));
                    }
                    let published = context.evaluate(&self.eval_context, |evaluator| {
                        evaluator.with_value_access(|access| {
                            let assignment = access.clone_root(&value);
                            handle.publish(access.values(), Ok(assignment))
                        })
                    });
                    let published = published
                        .map_err(|_| TaskHalt::new("reflection fixpoint initialized twice"))?;
                    published.notify();
                    branch.control.sequence.pop();
                    branch.active_fixes.pop();
                    Ok(MachineStep::Continue(MachineWork::deliver_root(
                        value,
                        branch,
                        scope_depth,
                    )))
                }
                Continuation::CloseScope(close) => {
                    branch.control.sequence.pop();
                    branch
                        .control
                        .sequence
                        .push(Continuation::RestoreScopedValue(value));
                    branch.set_effect_root(close);
                    Ok(MachineStep::Continue(MachineWork::Drive {
                        branch,
                        scope_depth,
                    }))
                }
                Continuation::RestoreScopedValue(scoped_value) => {
                    Ok(MachineStep::Demand(ScalarDemandWork::new(
                        value,
                        ScalarDemandPurpose::RestoreScopedValue { scoped_value },
                        branch,
                        scope_depth,
                    )))
                }
            };
        }

        let stack = context.evaluate(&self.eval_context, |evaluator| {
            reset_stack_root_in(evaluator, &branch.state, &self.tags.continuation_state)
        })?;
        Ok(MachineStep::Control(Box::new(ControlWork::delivery(
            stack,
            value,
            branch,
            scope_depth,
        ))))
    }

    fn enter_cut(
        &mut self,
        context: &mut EvaluationPollContext,
        operation: RuntimeValueRoot,
        mut outer: Branch<S>,
        parent_scope_depth: usize,
    ) -> MachineWork<S> {
        let outer_sequence = std::mem::take(&mut outer.control.sequence);
        debug_assert_eq!(operation.runtime_id(), outer.effect.runtime_id());
        let outermost = self.execution.cuts.is_empty();
        let mut frame = CutFrame {
            operation,
            outer,
            outer_sequence,
            parent_scope_depth,
            scope_depth: parent_scope_depth + 1,
            owns_transaction: false,
            alternatives: Vec::new(),
            fallback: None,
            retry: None,
            observed_failure: false,
            trying: false,
        };
        // Where the host runs the cuts, it owns their transactions.
        frame.owns_transaction = self.hosted.is_none() && frame.outer.transaction.is_none();
        self.begin_cut_attempt(context, &mut frame);
        let work = frame.next_alternative();
        let entry = frame.outer.host_version;
        self.execution.cuts.push(frame);
        // A failure returns to the cut's entry, which is set aside.
        if let Some(hosted) = &mut self.hosted {
            hosted.set_aside = true;
            if outermost {
                hosted.pending = Some(HostControl::CutBegin(entry));
            }
        }
        work
    }

    fn begin_cut_attempt(&mut self, context: &mut EvaluationPollContext, frame: &mut CutFrame<S>) {
        frame.alternatives.clear();
        frame.retry = None;
        frame.observed_failure = false;
        if frame.owns_transaction {
            let snapshot = context.suspend(|| self.host.snapshot());
            frame.outer.transaction = Some(Transaction::new(snapshot));
        }
        let mut initial = frame
            .outer
            .clone()
            .with_effect_root(frame.operation.clone());
        initial.control.sequence.clear();
        frame.alternatives.push(initial);
    }

    fn handle_outcome(
        &mut self,
        context: &mut EvaluationPollContext,
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        if self.execution.cuts.is_empty() {
            return self.handle_top_level_outcome(context, outcome, scope_depth);
        }
        let expected_scope = self
            .execution
            .cuts
            .last()
            .expect("checked nonempty cut stack")
            .scope_depth;
        if scope_depth != expected_scope {
            return Err(TaskHalt::new(
                "reflection cut stack became unbalanced during polling",
            ));
        }

        match outcome {
            BranchOutcome::Complete(value, mut completed) => {
                let owns_transaction = self
                    .execution
                    .cuts
                    .last()
                    .expect("checked nonempty cut stack")
                    .owns_transaction;
                if owns_transaction {
                    let transaction = completed
                        .transaction
                        .as_ref()
                        .expect("outer cut must own a transaction");
                    let commit = TaskCommit::new(
                        transaction.store.clone(),
                        transaction.snapshot.extra().clone(),
                        transaction.journal.clone(),
                    );
                    match context.suspend(|| self.host.commit(commit)) {
                        CommitResult::Committed => {
                            completed.transaction = None;
                        }
                        CommitResult::Conflict => {
                            let frame = self
                                .execution
                                .cuts
                                .last_mut()
                                .expect("checked nonempty cut stack");
                            if frame.trying {
                                let frame = self
                                    .execution
                                    .cuts
                                    .pop()
                                    .expect("checked nonempty cut stack");
                                return Ok(MachineStep::Continue(self.answer_try(
                                    context,
                                    frame,
                                    TryOutcome::Conflict,
                                    TryTrace::default(),
                                )));
                            }
                            frame.observed_failure = true;
                            frame.retry = Some(completed);
                            return self.finish_cut_attempt(context);
                        }
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => {
                            let parent_scope = self
                                .execution
                                .cuts
                                .pop()
                                .expect("checked nonempty cut stack")
                                .parent_scope_depth;
                            return Ok(MachineStep::Continue(MachineWork::Outcome {
                                outcome: BranchOutcome::Cancelled,
                                scope_depth: parent_scope,
                            }));
                        }
                    }
                }
                let frame = self
                    .execution
                    .cuts
                    .pop()
                    .expect("checked nonempty cut stack");
                let parent_scope_depth = frame.parent_scope_depth;
                // A `.try` answers with its operation's value as `ok`.
                let value = if frame.trying {
                    context.with_runtime_access(|access| {
                        let value = value.clone_core_with(&access);
                        completed.root_value(&access, try_value_in(&access, &keys::OK, value))
                    })
                } else {
                    value
                };
                if let Some(hosted) = self
                    .hosted
                    .as_mut()
                    .filter(|_| self.execution.cuts.is_empty())
                {
                    // The host commits the outermost cut, and may restart it.
                    completed.control.sequence = frame.outer_sequence.clone();
                    let retries = !frame.trying;
                    hosted.committing = Some(frame);
                    hosted.pending = Some(HostControl::CutCommit { retries });
                } else {
                    completed.control.sequence = frame.outer_sequence;
                }
                Ok(MachineStep::Continue(MachineWork::deliver_root(
                    value,
                    completed,
                    parent_scope_depth,
                )))
            }
            BranchOutcome::Fork(left, right) => {
                let frame = self
                    .execution
                    .cuts
                    .last_mut()
                    .expect("checked nonempty cut stack");
                frame.alternatives.push(*right);
                frame.alternatives.push(*left);
                Ok(MachineStep::Continue(frame.next_alternative()))
            }
            BranchOutcome::Fail(mut failed) | BranchOutcome::Retry(mut failed) => {
                if let Some(restarted) =
                    self.restart_fixpoint_at_scope(context, &mut failed, scope_depth)?
                {
                    return Ok(MachineStep::Control(Box::new(restarted)));
                }
                let frame = self
                    .execution
                    .cuts
                    .last_mut()
                    .expect("checked nonempty cut stack");
                frame.observed_failure |= failed
                    .transaction
                    .as_ref()
                    .is_some_and(|transaction| transaction.observed);
                frame.retry = Some(failed);
                if !frame.alternatives.is_empty() {
                    let work = frame.next_alternative();
                    if let Some(branch) = work.branch() {
                        self.hosted_resume(branch.host_version);
                    }
                    return Ok(MachineStep::Continue(work));
                }
                self.finish_cut_attempt(context)
            }
            BranchOutcome::Cancelled => {
                let parent_scope = self
                    .execution
                    .cuts
                    .pop()
                    .expect("checked nonempty cut stack")
                    .parent_scope_depth;
                Ok(MachineStep::Continue(MachineWork::Outcome {
                    outcome: BranchOutcome::Cancelled,
                    scope_depth: parent_scope,
                }))
            }
        }
    }

    /// Answers a `.try`, whose `frame` has left the stack, with `outcome`.
    fn answer_try(
        &mut self,
        context: &EvaluationPollContext,
        frame: CutFrame<S>,
        outcome: TryOutcome,
        trace: TryTrace,
    ) -> MachineWork<S> {
        let value = context.with_runtime_access(|access| {
            frame
                .outer
                .root_value(&access, try_outcome_in(&access, outcome))
        });
        self.deliver_try(frame, value, trace)
    }

    /// Continues where a `.try`, whose `frame` has left the stack, began,
    /// with `value`, what it answers. What its operation did is undone; its
    /// `trace`, what it observed and waited on, stays with the transaction
    /// around it.
    fn deliver_try(
        &mut self,
        frame: CutFrame<S>,
        value: RuntimeValueRoot,
        trace: TryTrace,
    ) -> MachineWork<S> {
        let mut branch = frame.outer;
        branch.control.sequence = frame.outer_sequence;
        if frame.owns_transaction {
            branch.transaction = None;
        } else if let Some(transaction) = branch.transaction.as_mut() {
            transaction.observed |= trace.observed;
            // A promise it waited on is observed, as `.promise.status`
            // reads one: its resolution wakes a cut that fails on the
            // `wait`, however many promises the cut waited on.
            if let Some(promise) = trace.waited {
                transaction.observed |= transaction.store.observe_promise_read(promise);
            }
        }
        MachineWork::deliver_root(value, branch, frame.parent_scope_depth)
    }

    fn finish_cut_attempt(
        &mut self,
        context: &mut EvaluationPollContext,
    ) -> Result<MachineStep<S>, TaskHalt> {
        let mut frame = self
            .execution
            .cuts
            .pop()
            .expect("cut attempt requires a cut frame");
        if let Some(hosted) = self
            .hosted
            .as_mut()
            .filter(|_| self.execution.cuts.is_empty())
        {
            // The host returns to where the outermost cut began.
            let entry = frame.outer.host_version;
            hosted.version = entry;
            hosted.pending = Some(HostControl::CutAbort(entry));
        }
        let mut failed = frame.retry.take().unwrap_or_else(|| frame.outer.clone());
        if frame.observed_failure
            && let Some(transaction) = failed.transaction.as_mut()
        {
            transaction.observed = true;
        }
        if failed
            .fix_restarts
            .last()
            .is_some_and(|restart| restart.root.scope_depth < frame.scope_depth)
        {
            return Ok(MachineStep::Continue(MachineWork::Outcome {
                outcome: BranchOutcome::Retry(failed),
                scope_depth: frame.parent_scope_depth,
            }));
        }
        // A `.try` answers with its operation's failure.
        if frame.trying {
            let trace = TryTrace {
                observed: frame.observed_failure
                    || failed
                        .transaction
                        .as_ref()
                        .is_some_and(|transaction| transaction.observed),
                waited: None,
            };
            if !self.execution.cuts.is_empty() {
                self.hosted_resume(frame.outer.host_version);
            }
            return Ok(MachineStep::Continue(self.answer_try(
                context,
                frame,
                TryOutcome::Fail,
                trace,
            )));
        }
        // A failed cut is a failure, which its choice handles by falling
        // back; only a failure nothing handles waits for a change.
        if let Some(fallback) = frame.fallback {
            return Ok(MachineStep::Continue(MachineWork::Drive {
                branch: fallback,
                scope_depth: frame.parent_scope_depth,
            }));
        }
        if !frame.owns_transaction {
            return Ok(MachineStep::Continue(MachineWork::Outcome {
                outcome: failed.into_failure(),
                scope_depth: frame.parent_scope_depth,
            }));
        }
        if !frame.observed_failure {
            failed.transaction = None;
            return Ok(MachineStep::Continue(MachineWork::Outcome {
                outcome: failed.into_failure(),
                scope_depth: frame.parent_scope_depth,
            }));
        }

        let generation = failed
            .transaction
            .as_ref()
            .map(|transaction| transaction.snapshot.generation())
            .unwrap_or_else(|| context.suspend(|| self.host.snapshot().generation()));
        let validation = failed.transaction.clone();
        frame.retry = Some(failed);
        let index = self.execution.cuts.len();
        self.execution.cuts.push(frame);
        Ok(MachineStep::Blocked(BlockedExecution::exhausted(
            RetryWake {
                observed_generation: generation,
                validation,
                action: WakeAction::RestartCut(index),
            },
        )))
    }

    fn handle_top_level_outcome(
        &mut self,
        context: &EvaluationPollContext,
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        if self.search.retains_all() {
            return self.handle_isolated_search_outcome(context, outcome, scope_depth);
        }
        if self.hosted.is_some() {
            return self.handle_hosted_outcome(context, outcome, scope_depth);
        }
        match outcome {
            BranchOutcome::Complete(value, _) => Ok(MachineStep::Terminal(TaskTerminal::Complete(
                PublicValue::from_runtime_root(value),
            ))),
            BranchOutcome::Fail(_) => Ok(MachineStep::Terminal(TaskTerminal::Failed(
                TaskHalt::new("reflection task failed permanently"),
            ))),
            BranchOutcome::Fork(_, _) => Ok(MachineStep::Terminal(TaskTerminal::Failed(
                TaskHalt::new("a choice forked outside every cut"),
            ))),
            BranchOutcome::Retry(mut failed) => {
                if let Some(restarted) =
                    self.restart_fixpoint_at_scope(context, &mut failed, scope_depth)?
                {
                    return Ok(MachineStep::Control(Box::new(restarted)));
                }
                let checkpoint = failed.retry.take().ok_or_else(|| {
                    TaskHalt::new("retryable reflection failure lost its observation")
                })?;
                let generation = checkpoint.generation.ok_or_else(|| {
                    TaskHalt::new("retryable reflection failure lost its wake generation")
                })?;
                Ok(MachineStep::Blocked(BlockedExecution::exhausted(
                    RetryWake {
                        observed_generation: generation,
                        validation: failed.transaction.clone(),
                        action: WakeAction::ReplaceWork(Box::new(MachineWork::Drive {
                            branch: *checkpoint.branch,
                            scope_depth,
                        })),
                    },
                )))
            }
            BranchOutcome::Cancelled => Ok(MachineStep::Terminal(TaskTerminal::Cancelled)),
        }
    }

    /// An outcome outside every cut of a task whose host runs its cuts:
    /// completion, or a failure nothing handled, which the host answers.
    fn handle_hosted_outcome(
        &mut self,
        context: &EvaluationPollContext,
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        let (result, terminal) = match outcome {
            BranchOutcome::Complete(value, completed) => (
                HostedResult::Complete(PublicValue::from_runtime_root(completed.state)),
                TaskTerminal::Complete(PublicValue::from_runtime_root(value)),
            ),
            BranchOutcome::Fail(mut failed) | BranchOutcome::Retry(mut failed) => {
                if let Some(restarted) =
                    self.restart_fixpoint_at_scope(context, &mut failed, scope_depth)?
                {
                    return Ok(MachineStep::Control(Box::new(restarted)));
                }
                (
                    HostedResult::Backtracked,
                    TaskTerminal::Failed(TaskHalt::new("the effect failed")),
                )
            }
            BranchOutcome::Fork(_, _) => {
                return Err(TaskHalt::new("a choice forked outside every cut"));
            }
            BranchOutcome::Cancelled => return Ok(MachineStep::Terminal(TaskTerminal::Cancelled)),
        };
        self.hosted
            .as_mut()
            .expect("a hosted outcome has its host's record")
            .result = Some(result);
        Ok(MachineStep::Terminal(terminal))
    }

    /// Where the host runs the cuts, notes a branch set aside, to which
    /// the host must be able to return.
    fn hosted_set_aside(&mut self) {
        if let Some(hosted) = &mut self.hosted {
            hosted.set_aside = true;
        }
    }

    /// Where the host runs the cuts, raises `event`, which the task pauses
    /// for before its next step.
    fn hosted_signal(&mut self, event: HostControl) {
        let hosted = self
            .hosted
            .as_mut()
            .expect("only a task whose host runs its cuts signals it");
        debug_assert!(hosted.pending.is_none(), "one host event a step");
        hosted.pending = Some(event);
    }

    /// Where the host runs the cuts, notes the resumption of a branch set
    /// aside at `version`: the host returns to that version first.
    fn hosted_resume(&mut self, version: u64) {
        if self
            .hosted
            .as_ref()
            .is_some_and(|hosted| version < hosted.version)
        {
            self.hosted.as_mut().expect("checked above").version = version;
            self.hosted_signal(HostControl::Restore(version));
        }
    }

    /// Pauses before the next step for the event the last step raised.
    fn hosted_pause(&mut self) -> Option<EffectTaskPoll> {
        let hosted = self.hosted.as_mut()?;
        if let Some(event) = hosted.pending.take() {
            return Some(EffectTaskPoll::Paused(event));
        }
        assert!(
            hosted.committing.is_none(),
            "a hosted cut's commit awaits its host's answer"
        );
        // Each resumption of an earlier version raises a restore.
        debug_assert!(
            self.execution
                .driven_branch()
                .is_none_or(|branch| branch.host_version == hosted.version),
            "a hosted task drives a branch of its host's present version"
        );
        None
    }

    /// Whether the host must keep a way back to its state before it
    /// answers a request.
    pub(super) fn host_needs_checkpoint(&self) -> bool {
        self.hosted.as_ref().is_some_and(|hosted| hosted.set_aside)
    }

    /// The count of requests the host has answered.
    pub(super) fn host_version(&self) -> u64 {
        self.hosted.as_ref().map_or(0, |hosted| hosted.version)
    }

    /// How the task ended, once it has.
    pub(super) fn take_hosted_result(&mut self) -> Option<HostedResult> {
        self.hosted.as_mut()?.result.take()
    }

    /// The host's answer to the outermost cut's commit: committed, or, as
    /// the host began its transaction again, the cut runs again.
    pub(super) fn resume_commit(&mut self, committed: bool) {
        let frame = self
            .hosted
            .as_mut()
            .and_then(|hosted| hosted.committing.take())
            .expect("a commit awaits its host's answer");
        if committed {
            return;
        }
        if frame.trying {
            // The host ended its transaction, returning to where the `.try`
            // began, which answers with the conflict in place of its
            // operation's value.
            self.hosted
                .as_mut()
                .expect("only a host that runs the cuts commits one")
                .version = frame.outer.host_version;
            let values = self.eval_context.values().clone();
            let value = values.with_boundary_access(|access| {
                frame
                    .outer
                    .root_value(&access, try_outcome_in(&access, TryOutcome::Conflict))
            });
            self.execution.work = self.deliver_try(frame, value, TryTrace::default());
            return;
        }
        self.restart_hosted_cut(frame);
    }

    /// Runs the outermost cut again from its entry, as the host began its
    /// transaction again where the cut diverged.
    pub(super) fn restart_outermost_cut(&mut self) {
        let frame = self
            .execution
            .cuts
            .drain(..)
            .next()
            .expect("an outermost cut to restart");
        self.restart_hosted_cut(frame);
    }

    fn restart_hosted_cut(&mut self, mut frame: CutFrame<S>) {
        self.execution.cuts.clear();
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.controlling = None;
        self.execution.specializing = None;
        self.blocked = None;
        let hosted = self
            .hosted
            .as_mut()
            .expect("only a host that runs the cuts restarts one");
        hosted.pending = None;
        hosted.version = frame.outer.host_version;
        hosted.set_aside = true;
        frame.alternatives.clear();
        frame.retry = None;
        frame.observed_failure = false;
        let mut initial = frame
            .outer
            .clone()
            .with_effect_root(frame.operation.clone());
        initial.control.sequence.clear();
        frame.alternatives.push(initial);
        self.execution.work = frame.next_alternative();
        self.execution.cuts.push(frame);
    }

    fn handle_isolated_search_outcome(
        &mut self,
        context: &EvaluationPollContext,
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        debug_assert_eq!(scope_depth, 0);
        match outcome {
            BranchOutcome::Complete(value, mut completed) => {
                let restarted =
                    self.restart_fixpoint_at_scope(context, &mut completed, scope_depth)?;
                let transaction = Self::isolated_transaction(&mut completed);
                self.search.retain(IsolatedSearchBranch::complete(
                    PublicValue::from_runtime_root(value),
                    transaction,
                ));
                Ok(restarted
                    .map(|work| MachineStep::Control(Box::new(work)))
                    .unwrap_or_else(|| self.advance_isolated_search(context)))
            }
            BranchOutcome::Fork(left, right) => {
                let branch = self
                    .search
                    .fork(*left, *right)
                    .expect("isolated search must accept an outer alternative");
                Ok(MachineStep::Continue(MachineWork::Drive {
                    branch,
                    scope_depth: 0,
                }))
            }
            BranchOutcome::Fail(mut failed) => {
                if let Some(restarted) =
                    self.restart_fixpoint_at_scope(context, &mut failed, scope_depth)?
                {
                    return Ok(MachineStep::Control(Box::new(restarted)));
                }
                let transaction = Self::isolated_transaction(&mut failed);
                self.search
                    .retain(IsolatedSearchBranch::failed(transaction));
                Ok(self.advance_isolated_search(context))
            }
            BranchOutcome::Retry(mut failed) => {
                if let Some(restarted) =
                    self.restart_fixpoint_at_scope(context, &mut failed, scope_depth)?
                {
                    return Ok(MachineStep::Control(Box::new(restarted)));
                }
                let generation = failed
                    .transaction
                    .as_ref()
                    .filter(|transaction| transaction.observed)
                    .map(|transaction| transaction.snapshot.generation())
                    .or_else(|| {
                        failed
                            .retry
                            .as_ref()
                            .and_then(|checkpoint| checkpoint.generation)
                    })
                    .ok_or_else(|| {
                        TaskHalt::new("retryable isolated branch lost its wake generation")
                    })?;
                Ok(MachineStep::Blocked(BlockedExecution::exhausted(
                    RetryWake {
                        observed_generation: generation,
                        validation: failed.transaction.clone(),
                        action: WakeAction::RestartSearch,
                    },
                )))
            }
            BranchOutcome::Cancelled => Ok(MachineStep::Terminal(TaskTerminal::Cancelled)),
        }
    }

    fn advance_isolated_search(&mut self, context: &EvaluationPollContext) -> MachineStep<S> {
        if let Some(branch) = self.search.next_alternative() {
            MachineStep::Continue(MachineWork::Drive {
                branch,
                scope_depth: 0,
            })
        } else {
            self.search.finish();
            MachineStep::Terminal(TaskTerminal::Complete(context.with_runtime_access(
                |access| PublicValue::from_runtime_root(access.root_runtime_value(access.unit())),
            )))
        }
    }

    fn isolated_transaction(branch: &mut Branch<S>) -> TaskCommit<S> {
        let transaction = branch
            .transaction
            .take()
            .expect("an isolated outer branch must retain its transaction");
        TaskCommit::new(
            transaction.store,
            transaction.snapshot.extra().clone(),
            transaction.journal,
        )
    }

    fn retry_wake(&self) -> Option<RetryWake<S>> {
        if let Some(index) = self
            .execution
            .cuts
            .iter()
            .rposition(|frame| frame.owns_transaction)
        {
            let frame_observed = self.execution.cuts[index..]
                .iter()
                .any(|frame| frame.observed_failure);
            let branch_observed = self
                .execution
                .active_branch()
                .and_then(|branch| branch.transaction.as_ref())
                .is_some_and(|transaction| transaction.observed);
            if frame_observed || branch_observed {
                let validation = self
                    .execution
                    .active_branch()
                    .and_then(|branch| branch.transaction.clone())
                    .or_else(|| self.execution.cuts[index].outer.transaction.clone());
                let generation = validation
                    .as_ref()
                    .map(|transaction| transaction.snapshot.generation());
                if let Some(generation) = generation {
                    return Some(RetryWake {
                        observed_generation: generation,
                        validation,
                        action: WakeAction::RestartCut(index),
                    });
                }
            }
        }
        if self.search.retains_all()
            && let Some(transaction) = self
                .execution
                .active_branch()
                .and_then(|branch| branch.transaction.as_ref())
            && transaction.observed
        {
            return Some(RetryWake {
                observed_generation: transaction.snapshot.generation(),
                validation: Some(transaction.clone()),
                action: WakeAction::RestartSearch,
            });
        }
        let branch = self.execution.active_branch()?;
        let checkpoint = branch.retry.as_ref()?;
        let observed_generation = checkpoint.generation?;
        Some(RetryWake {
            observed_generation,
            validation: branch.transaction.clone(),
            action: WakeAction::ReplaceWork(Box::new(MachineWork::Drive {
                branch: (*checkpoint.branch).clone(),
                scope_depth: self.execution.active_scope_depth(),
            })),
        })
    }

    /// As [`Self::install_blocked`], for a phase that blocked: within a
    /// `.try`, a block that rests on a promise no one has resolved is the
    /// `.try`'s answer (`api-refl-try`).
    fn install_blocked_in(
        &mut self,
        context: &mut EvaluationPollContext,
        blocked: BlockedExecution<S>,
    ) -> Option<EffectTaskPoll> {
        let poll = self.install_blocked(context, blocked)?;
        if matches!(poll, EffectTaskPoll::Blocked(_)) && self.try_answers_wait(context) {
            return None;
        }
        Some(poll)
    }

    /// The innermost open `.try`'s frame, by its index.
    fn innermost_try(&self) -> Option<usize> {
        self.execution.cuts.iter().rposition(|frame| frame.trying)
    }

    /// Within a `.try`, answers `wait` where the task's block rests on a
    /// promise of the store no one has resolved, directly or through work
    /// the coordinator knows waits on it. It runs nothing: the poll's own
    /// pumping, and the scheduler's, run the work the task waits on, and
    /// the task asks again when polled; lazy work in progress is waited for,
    /// and a panicked producer halts the task, as outside a `.try`.
    /// Whether it answered.
    fn try_answers_wait(&mut self, context: &EvaluationPollContext) -> bool {
        // A host that runs the task's cuts parks the requests it serves on
        // promises it resolves, so only it knows which waits are outside
        // the task's control; it says so with `answer_try_wait`.
        if self.hosted.is_some() {
            return false;
        }
        let Some(index) = self.innermost_try() else {
            return false;
        };
        let Some(BlockedExecution {
            reason: BlockReason::WaitingOn(dependency),
            ..
        }) = &self.blocked
        else {
            return false;
        };
        if dependency.panic_report().is_some() {
            return false;
        }
        let Some(promise) = self.awaited_store_promise(index, dependency) else {
            return false;
        };
        self.unwind_to_try(context, index, TryOutcome::Wait, Some(promise));
        true
    }

    /// The store's pending promise that `dependency` rests on, directly or
    /// through blocked work, as seen from the `.try` whose frame is at
    /// `index`. A promise internal to evaluation is work in progress, not
    /// what a `.try` waits on.
    fn awaited_store_promise(
        &self,
        index: usize,
        dependency: &WorkDependency,
    ) -> Option<PromiseId> {
        let cell = match dependency {
            WorkDependency::Promise(promise) => promise.id(),
            dependency => self
                .eval_context
                .values()
                .work_coordinator()?
                .awaited_promise(dependency)?,
        };
        let transaction = self.execution.cuts[index].outer.transaction.as_ref()?;
        transaction
            .snapshot
            .store()
            .promises()
            .is_pending(cell)
            .then_some(cell)
    }

    /// Answers the innermost `.try` with `wait`, where its host found the
    /// task's block outside the task's control: not a request it serves.
    /// Whether a `.try` was open to answer.
    pub(super) fn answer_try_wait(&mut self) -> bool {
        let Some(index) = self.innermost_try() else {
            return false;
        };
        let (frame, trace) = self.leave_try(index);
        let values = self.eval_context.values().clone();
        let value = values.with_boundary_access(|access| {
            frame
                .outer
                .root_value(&access, try_outcome_in(&access, TryOutcome::Wait))
        });
        self.execution.work = self.deliver_try(frame, value, trace);
        true
    }

    /// Leaves the `.try` whose frame is at `index`, and every frame within
    /// it, discarding the work in progress, and answers it with `outcome`.
    fn unwind_to_try(
        &mut self,
        context: &EvaluationPollContext,
        index: usize,
        outcome: TryOutcome,
        waited: Option<PromiseId>,
    ) {
        let (frame, mut trace) = self.leave_try(index);
        trace.waited = waited;
        self.execution.work = self.answer_try(context, frame, outcome, trace);
    }

    /// Leaves the `.try` whose frame is at `index`, and every frame within
    /// it, discarding the work in progress: its frame, and what it observed
    /// and waited on.
    fn leave_try(&mut self, index: usize) -> (CutFrame<S>, TryTrace) {
        let active = self
            .execution
            .active_branch()
            .and_then(|branch| branch.transaction.as_ref());
        let trace = TryTrace {
            observed: self.execution.cuts[index..]
                .iter()
                .any(|frame| frame.observed_failure)
                || active.is_some_and(|transaction| transaction.observed),
            waited: None,
        };
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.controlling = None;
        self.execution.specializing = None;
        self.blocked = None;
        self.execution.cuts.truncate(index + 1);
        let frame = self
            .execution
            .cuts
            .pop()
            .expect("the `.try`'s frame is on the stack");
        let entry = frame.outer.host_version;
        if self.execution.cuts.is_empty() {
            if let Some(hosted) = self.hosted.as_mut() {
                // The host returns to where the outermost cut began.
                hosted.version = entry;
                hosted.pending = Some(HostControl::CutAbort(entry));
            }
        } else {
            self.hosted_resume(entry);
        }
        (frame, trace)
    }

    fn install_blocked(
        &mut self,
        context: &mut EvaluationPollContext,
        blocked: BlockedExecution<S>,
    ) -> Option<EffectTaskPoll> {
        self.blocked = Some(blocked);
        // A resumed machine may have acquired a new transactional observation
        // after the publication which woke its previous dependency. Validate
        // the complete read set before exposing the new blocked state; no
        // later publication is required to rescue a stale subscription.
        match self.validate_blocked_retry(context) {
            BlockedRetryPoll::Stable => Some(self.blocked_poll()),
            BlockedRetryPoll::Restarted => None,
            BlockedRetryPoll::Terminal(poll) => Some(poll),
        }
    }

    fn validate_blocked_retry(&mut self, context: &mut EvaluationPollContext) -> BlockedRetryPoll {
        let retry_validation = self
            .blocked
            .as_ref()
            .expect("blocked validation requires installed blocked work")
            .retry
            .as_ref()
            .and_then(|retry| {
                retry.validation.as_ref().map(|transaction| {
                    context.suspend(|| self.host.validate(transaction.validation()))
                })
            });
        match retry_validation {
            Some(ValidationResult::Current { generation }) => {
                self.blocked
                    .as_mut()
                    .and_then(|blocked| blocked.retry.as_mut())
                    .expect("validated blocked work must retain its retry capsule")
                    .observed_generation = generation;
                BlockedRetryPoll::Stable
            }
            Some(ValidationResult::Conflict) => {
                let retry = self
                    .blocked
                    .take()
                    .and_then(|blocked| blocked.retry)
                    .expect("conflicting blocked work must retain its wake action");
                self.apply_wake(context, retry.action);
                BlockedRetryPoll::Restarted
            }
            Some(ValidationResult::MissingVolume(volume)) => {
                let error = context
                    .with_runtime_access(|access| missing_volume_error(volume).root_in(&access));
                self.finish_in(context, TaskTerminal::Failed(error));
                BlockedRetryPoll::Terminal(
                    self.terminal.as_ref().expect("terminal set above").poll(),
                )
            }
            Some(ValidationResult::Closed) => {
                self.finish_in(context, TaskTerminal::Cancelled);
                BlockedRetryPoll::Terminal(
                    self.terminal.as_ref().expect("terminal set above").poll(),
                )
            }
            None => {
                let changed = self.blocked.as_ref().is_some_and(|blocked| {
                    blocked.retry.as_ref().is_some_and(|retry| {
                        context.suspend(|| self.host.snapshot().generation())
                            != retry.observed_generation
                    })
                });
                if changed {
                    let retry = self
                        .blocked
                        .take()
                        .and_then(|blocked| blocked.retry)
                        .expect("changed retry generation must retain its wake action");
                    self.apply_wake(context, retry.action);
                    BlockedRetryPoll::Restarted
                } else {
                    BlockedRetryPoll::Stable
                }
            }
        }
    }

    fn poll_blocked(
        &mut self,
        context: &mut EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> Option<EffectTaskPoll> {
        self.blocked.as_ref()?;
        match self.validate_blocked_retry(context) {
            BlockedRetryPoll::Stable => {}
            BlockedRetryPoll::Restarted => return None,
            BlockedRetryPoll::Terminal(poll) => return Some(poll),
        }
        if self.try_answers_wait(context) {
            return None;
        }
        let blocked = self.blocked.as_ref().expect("checked blocked state above");
        let BlockReason::WaitingOn(dependency) = &blocked.reason else {
            return Some(self.blocked_poll());
        };
        // A panicked dependency stays blocked: re-polling would reinstall
        // the panicked work, so the scheduler's poll boundary halts this task.
        if dependency.panic_report().is_some() {
            return Some(self.blocked_poll());
        }
        if dependency.is_terminal() {
            self.blocked = None;
            return None;
        }
        let WorkDependency::Wait(wait) = dependency else {
            return Some(self.blocked_poll());
        };
        match self.eval_context.poll_wait(wait) {
            EvaluationWaitPoll::Pending(_) => {
                // Pumped work shares this poll's mutator.
                let pumped = self.eval_context.pump_wait_on_route_within(
                    context,
                    wait,
                    step_budget,
                    &mut self.exact_demand_route,
                );
                if matches!(pumped, EvaluationPumpOutcome::TargetReady) {
                    self.blocked = None;
                    None
                } else if self.try_answers_wait(context) {
                    None
                } else {
                    Some(self.blocked_poll())
                }
            }
            EvaluationWaitPoll::Panicked(_) => Some(self.blocked_poll()),
            EvaluationWaitPoll::Complete(_)
            | EvaluationWaitPoll::Failed(_)
            | EvaluationWaitPoll::Cancelled
            | EvaluationWaitPoll::Abandoned
            | EvaluationWaitPoll::Exited
            | EvaluationWaitPoll::Killed(_) => {
                self.blocked = None;
                None
            }
        }
    }

    fn poll_exit(&mut self, context: &mut EvaluationPollContext) -> Option<EffectTaskPoll> {
        let validation = self.exit.as_ref()?.restart.as_ref().and_then(|retry| {
            retry
                .validation
                .as_ref()
                .map(|transaction| context.suspend(|| self.host.validate(transaction.validation())))
        });
        match validation {
            Some(ValidationResult::Current { generation }) => {
                let exit = self.exit.as_mut().expect("checked exit state above");
                exit.restart
                    .as_mut()
                    .expect("validated exit must retain its retry capsule")
                    .observed_generation = generation;
                exit.poll.observed_generation = Some(generation);
                return Some(EffectTaskPoll::Exit(exit.poll.clone()));
            }
            Some(ValidationResult::Conflict) => {}
            Some(ValidationResult::MissingVolume(volume)) => {
                let error = context
                    .with_runtime_access(|access| missing_volume_error(volume).root_in(&access));
                self.finish_in(context, TaskTerminal::Failed(error));
                return Some(self.terminal.as_ref().expect("terminal set above").poll());
            }
            Some(ValidationResult::Closed) => {
                self.finish_in(context, TaskTerminal::Cancelled);
                return Some(self.terminal.as_ref().expect("terminal set above").poll());
            }
            None => {
                let exit = self.exit.as_ref().expect("checked exit state above");
                let changed = exit.restart.as_ref().is_some_and(|retry| {
                    context.suspend(|| self.host.snapshot().generation())
                        != retry.observed_generation
                });
                if !changed {
                    return Some(EffectTaskPoll::Exit(exit.poll.clone()));
                }
            }
        }

        let retry = self
            .exit
            .take()
            .and_then(|exit| exit.restart)
            .expect("a changed exit observation must retain its restart capsule");
        self.apply_wake(context, retry.action);
        None
    }

    fn prepare_exit(&mut self, intent: ExitIntent) -> TaskExitState<S> {
        let restart = self.retry_wake();
        let poll = TaskExitBlock {
            intent,
            observed_generation: restart.as_ref().map(|restart| restart.observed_generation),
        };
        self.discard_exit_attempt(restart.as_ref());
        TaskExitState { poll, restart }
    }

    /// Retains only the control checkpoint named by `restart`. Every active
    /// branch, queued alternative, failed attempt, isolated result, and cloned
    /// transaction is dropped here so its pending task reservations and
    /// buffered host effects cannot survive the exit vote.
    fn discard_exit_attempt(&mut self, restart: Option<&RetryWake<S>>) {
        self.search.discard_progress();
        match restart.map(|restart| &restart.action) {
            Some(WakeAction::RestartCut(index)) => {
                assert_eq!(
                    *index, 0,
                    "the transaction-owning retry cut must be the outermost active cut"
                );
                let mut frame = self
                    .execution
                    .cuts
                    .get(*index)
                    .cloned()
                    .expect("exit retry must retain its cut checkpoint");
                frame.outer.transaction = None;
                frame.alternatives.clear();
                frame.retry = None;
                frame.observed_failure = false;
                self.execution.cuts.clear();
                self.execution.cuts.push(frame);
            }
            Some(WakeAction::ReplaceWork(_)) | Some(WakeAction::RestartSearch) | None => {
                self.execution.cuts.clear()
            }
        }
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.specializing = None;
        self.execution.work = MachineWork::Outcome {
            outcome: BranchOutcome::Cancelled,
            scope_depth: 0,
        };
    }

    fn blocked_poll(&self) -> EffectTaskPoll {
        let blocked = self
            .blocked
            .as_ref()
            .expect("blocked poll requires blocked state");
        EffectTaskPoll::Blocked(TaskBlock {
            dependency: blocked.dependency(),
            observed_generation: blocked.observed_generation(),
            error: blocked.error(),
        })
    }

    fn apply_wake(&mut self, context: &mut EvaluationPollContext, wake: WakeAction<S>) {
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.controlling = None;
        self.execution.specializing = None;
        match wake {
            WakeAction::ReplaceWork(work) => self.execution.work = *work,
            WakeAction::RestartCut(index) => self.restart_cut(context, index),
            WakeAction::RestartSearch => self.restart_search(context),
        }
    }

    fn restart_search(&mut self, context: &mut EvaluationPollContext) {
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.controlling = None;
        self.execution.specializing = None;
        self.execution.cuts.clear();
        let mut root = self
            .search
            .restart()
            .expect("only isolated search can restart its outer boundary");
        root.transaction = Some(Transaction::new(context.suspend(|| self.host.snapshot())));
        self.execution.work = MachineWork::Drive {
            branch: root,
            scope_depth: 0,
        };
    }

    fn restart_cut(&mut self, context: &mut EvaluationPollContext, index: usize) {
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.controlling = None;
        self.execution.specializing = None;
        self.execution.cuts.truncate(index + 1);
        let mut frame = self
            .execution
            .cuts
            .pop()
            .expect("blocked cut must remain on the cut stack");
        frame.outer.transaction = None;
        self.begin_cut_attempt(context, &mut frame);
        let work = frame.next_alternative();
        self.execution.cuts.push(frame);
        self.execution.work = work;
    }

    /// Cancels this task from teardown, outside any mutator.
    pub(super) fn cancel(&mut self) {
        let values = self.eval_context.values().clone();
        let wakes =
            values.with_boundary_access(|access| self.finish(&access, TaskTerminal::Cancelled));
        notify_publications(wakes);
    }

    /// Finishes this task through its quantum's access.
    fn finish_in(&mut self, context: &EvaluationPollContext, terminal: TaskTerminal) {
        let wakes = context.with_runtime_access(|access| self.finish(&access, terminal));
        notify_publications(wakes);
    }

    /// Finishes this task through its driver's mutator.
    fn finish_under(
        &mut self,
        mutator: &crate::core::RuntimeValueMutator<'_, '_>,
        terminal: TaskTerminal,
    ) {
        let wakes = mutator.with_access(|access| self.finish(&access, terminal));
        notify_publications(wakes);
    }

    /// Records `terminal`, rooting a failure through `access`, and fails the
    /// task's local promises. Returns their publications, which the caller
    /// notifies once `access` closes.
    #[must_use = "promise completion wakes must be delivered after managed access closes"]
    pub(super) fn finish(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        terminal: TaskTerminal,
    ) -> Vec<crate::core::ManagedPromisePublication> {
        if self.terminal.is_some() {
            return Vec::new();
        }
        let terminal = match terminal {
            TaskTerminal::Failed(error) => TaskTerminal::Failed(error.root_in(access)),
            terminal => terminal,
        };
        let unfinished_failure = match &terminal {
            TaskTerminal::Complete(_) => Arc::new(EvaluationFailure::message(
                "reflection task completed without fulfilling its fixpoint",
            )),
            TaskTerminal::Cancelled => Arc::new(EvaluationFailure::message(
                "reflection fixpoint producer was cancelled",
            )),
            TaskTerminal::Failed(error) => error.clone().into_failure(),
        };
        let wakes = self
            .eval_context
            .fail_local_promises(access, unfinished_failure);
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.controlling = None;
        self.execution.specializing = None;
        self.blocked = None;
        self.exit = None;
        self.terminal = Some(terminal);
        wakes
    }
}

fn notify_publications(publications: Vec<crate::core::ManagedPromisePublication>) {
    for publication in publications {
        publication.notify();
    }
}

fn effect_dispatch_context(access: &RuntimeValueAccess<'_>, stage: &str) -> Value {
    let stage_key = Key::binary_from_text("stage");
    let stage = Value::Atom(Atom::from_key(&Key::binary_from_text(stage)));
    crate::diagnostic::evaluation_context_frame_with_args_in(
        access,
        "effect_dispatch",
        Dict::new_sync().insert(stage_key, stage),
    )
}

pub(super) struct UnitEffectTask<S: TaskSpecialization>(pub(super) EffectTask<S>);

pub(super) struct ValueEffectTask<S: TaskSpecialization>(pub(super) EffectTask<S>);

pub(super) struct ContextualValueEffectTask<S: TaskSpecialization> {
    pub(super) task: EffectTask<S>,
    pub(super) context: RuntimeValueRoot,
}

impl<S: TaskSpecialization> ContextualValueEffectTask<S> {
    pub(super) fn new(task: EffectTask<S>, context: RuntimeValueRoot) -> Self {
        debug_assert_eq!(
            task.eval_context.values().runtime_id(),
            context.runtime_id()
        );
        Self { task, context }
    }
}

impl<S: TaskSpecialization> EvaluationTaskMachine for ValueEffectTask<S> {
    fn poll(
        &mut self,
        context: &mut crate::evaluation::EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        poll_value_effect_task(&mut self.0, context, step_budget)
    }

    fn cancel(&mut self) {
        self.0.cancel();
    }
}

impl<S: TaskSpecialization> EvaluationTaskMachine for ContextualValueEffectTask<S> {
    fn poll(
        &mut self,
        context: &mut crate::evaluation::EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        match poll_value_effect_task(&mut self.task, context, step_budget) {
            EvaluationMachinePoll::Failed(error) => {
                let failure = context.evaluate(&self.task.eval_context, |evaluator| {
                    evaluator.project_root(&self.context, |access, context| {
                        error.as_failure().with_context_in(
                            access.values(),
                            access.values().duplicate_value(&context),
                        )
                    })
                });
                EvaluationMachinePoll::Failed(context.root_failure(Arc::new(failure)))
            }
            poll => poll,
        }
    }

    fn cancel(&mut self) {
        self.task.cancel();
    }
}

fn poll_value_effect_task<S: TaskSpecialization>(
    task: &mut EffectTask<S>,
    context: &mut crate::evaluation::EvaluationPollContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> EvaluationMachinePoll {
    let observed_epoch = task.eval_context.current_observation_epoch();
    match task.poll_pumping(context, step_budget) {
        EffectTaskPoll::BudgetExhausted => EvaluationMachinePoll::BudgetExhausted,
        EffectTaskPoll::Blocked(blocked) => {
            if let Some(error) = blocked
                .dependency
                .as_ref()
                .and_then(|dependency| task.eval_context.recursive_promise_dependency(dependency))
            {
                let (poll, wakes) = context.with_runtime_access(|access| {
                    let error = TaskHalt::new(error).root_in(&access);
                    let wakes = task.finish(&access, TaskTerminal::Failed(error.clone()));
                    let poll = EvaluationMachinePoll::Failed(
                        access.root_runtime_failure(error.into_failure()),
                    );
                    (poll, wakes)
                });
                notify_publications(wakes);
                poll
            } else {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: blocked.dependency,
                    observed_epoch: blocked.observed_generation.map(|_| observed_epoch),
                    error: blocked.error,
                })
            }
        }
        EffectTaskPoll::Paused(_) => unreachable!("only a task whose host runs its cuts pauses"),
        EffectTaskPoll::Exit(exit) => EvaluationMachinePoll::Exit(EvaluationExitBlock {
            intent: exit.intent,
            observed_epoch: exit.observed_generation.map(|_| observed_epoch),
        }),
        EffectTaskPoll::Complete(value) => {
            EvaluationMachinePoll::Complete(value.into_runtime_root())
        }
        EffectTaskPoll::Failed(error) => {
            EvaluationMachinePoll::Failed(error.into_failure_root(task.eval_context.values()))
        }
        EffectTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
    }
}

impl<S: TaskSpecialization> EvaluationTaskMachine for UnitEffectTask<S> {
    fn poll(
        &mut self,
        context: &mut crate::evaluation::EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        let observed_epoch = self.0.eval_context.current_observation_epoch();
        match self.0.poll_with_context(context, step_budget) {
            EffectTaskPoll::BudgetExhausted => EvaluationMachinePoll::BudgetExhausted,
            EffectTaskPoll::Blocked(blocked) => {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: blocked.dependency,
                    observed_epoch: blocked.observed_generation.map(|_| observed_epoch),
                    error: blocked.error,
                })
            }
            EffectTaskPoll::Paused(_) => {
                unreachable!("only a task whose host runs its cuts pauses")
            }
            EffectTaskPoll::Exit(exit) => EvaluationMachinePoll::Exit(EvaluationExitBlock {
                intent: exit.intent,
                observed_epoch: exit.observed_generation.map(|_| observed_epoch),
            }),
            EffectTaskPoll::Complete(value) => {
                let value = value.into_runtime_root();
                let (is_unit, kind) = context.evaluate(&self.0.eval_context, |evaluator| {
                    evaluator.project_root(&value, |access, value| {
                        (
                            access
                                .values()
                                .same_representation(&value, &access.values().unit()),
                            access.values().diagnostic_kind_name(&value),
                        )
                    })
                });
                if is_unit {
                    EvaluationMachinePoll::Complete(value)
                } else {
                    EvaluationMachinePoll::Failed(context.root_failure(Arc::new(
                        EvaluationFailure::message(format!(
                            "effect task returned {kind}; expected unit"
                        )),
                    )))
                }
            }
            EffectTaskPoll::Failed(error) => {
                EvaluationMachinePoll::Failed(error.into_failure_root(self.0.eval_context.values()))
            }
            EffectTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
        }
    }

    fn cancel(&mut self) {
        self.0.cancel();
    }
}

#[derive(Clone)]
struct Branch<S: TaskSpecialization> {
    effect: RuntimeValueRoot,
    control: Control,
    state: RuntimeValueRoot,
    transaction: Option<Transaction<S>>,
    active_fixes: Vec<ActiveFix<S>>,
    fix_restarts: Vec<FixRestart<S>>,
    retry: Option<RetryCheckpoint<S>>,
    /// Where the host runs the cuts, the version of its state this branch
    /// continues from.
    host_version: u64,
}

impl<S: TaskSpecialization> Branch<S> {
    #[cfg(test)]
    fn new(values: &CoreValueFactory, effect: Value, state: Value) -> Self {
        let (effect, state) = values.with_boundary_access(|access| {
            (
                access.root_runtime_value(effect),
                access.root_runtime_value(state),
            )
        });
        Self::new_rooted(effect, state)
    }

    fn new_rooted(effect: RuntimeValueRoot, state: RuntimeValueRoot) -> Self {
        debug_assert_eq!(effect.runtime_id(), state.runtime_id());
        Self {
            effect,
            control: Control::default(),
            state,
            transaction: None,
            active_fixes: Vec::new(),
            fix_restarts: Vec::new(),
            retry: None,
            host_version: 0,
        }
    }

    fn with_effect_root(&self, effect: RuntimeValueRoot) -> Self {
        let mut branch = self.clone();
        branch.set_effect_root(effect);
        branch
    }

    fn set_effect_root(&mut self, effect: RuntimeValueRoot) {
        debug_assert_eq!(effect.runtime_id(), self.effect.runtime_id());
        self.effect = effect;
    }

    fn root_value(&self, access: &RuntimeValueAccess<'_>, value: Value) -> RuntimeValueRoot {
        debug_assert_eq!(access.runtime_id(), self.effect.runtime_id());
        access.root_runtime_value(value)
    }

    fn retry_candidate(&self) -> Option<Box<Self>> {
        if self.transaction.is_some() || self.retry.is_some() {
            return None;
        }
        let mut checkpoint = self.clone();
        checkpoint.retry = None;
        Some(Box::new(checkpoint))
    }

    fn observe(&mut self, checkpoint: Option<Box<Self>>, generation: u64) {
        if let Some(transaction) = self.transaction.as_mut() {
            transaction.observed = true;
        } else if self.retry.is_none()
            && let Some(branch) = checkpoint
        {
            self.retry = Some(RetryCheckpoint {
                generation: Some(generation),
                branch,
            });
        }
    }

    fn is_retryable(&self) -> bool {
        self.retry.is_some()
            || self
                .transaction
                .as_ref()
                .is_some_and(|transaction| transaction.observed)
    }

    fn into_failure(self) -> BranchOutcome<S> {
        if self.is_retryable() {
            BranchOutcome::Retry(self)
        } else {
            BranchOutcome::Fail(self)
        }
    }
}

struct TaskExecution<S: TaskSpecialization> {
    work: MachineWork<S>,
    decoding: Option<EffectDecodeWork<S>>,
    demanding: Option<ScalarDemandWork<S>>,
    pathing: Option<StatePathWork<S>>,
    controlling: Option<ControlWork<S>>,
    specializing: Option<Box<SpecializationWork<S>>>,
    cuts: Vec<CutFrame<S>>,
}

impl<S: TaskSpecialization> TaskExecution<S> {
    fn active_branch(&self) -> Option<&Branch<S>> {
        self.controlling
            .as_ref()
            .map(|work| &work.branch)
            .or_else(|| self.specializing.as_ref().map(|work| &work.branch))
            .or_else(|| self.demanding.as_ref().map(|work| &work.branch))
            .or_else(|| self.pathing.as_ref().map(|work| &work.branch))
            .or_else(|| self.decoding.as_ref().map(|work| &work.branch))
            .or_else(|| self.work.branch())
    }

    /// The branch the next step drives, which an outcome does not.
    fn driven_branch(&self) -> Option<&Branch<S>> {
        self.controlling
            .as_ref()
            .map(|work| &work.branch)
            .or_else(|| self.specializing.as_ref().map(|work| &work.branch))
            .or_else(|| self.demanding.as_ref().map(|work| &work.branch))
            .or_else(|| self.pathing.as_ref().map(|work| &work.branch))
            .or_else(|| self.decoding.as_ref().map(|work| &work.branch))
            .or_else(|| match &self.work {
                MachineWork::Outcome { .. } => None,
                work => work.branch(),
            })
    }

    fn active_scope_depth(&self) -> usize {
        self.controlling
            .as_ref()
            .map(|work| work.scope_depth)
            .or_else(|| self.specializing.as_ref().map(|work| work.scope_depth))
            .or_else(|| self.demanding.as_ref().map(|work| work.scope_depth))
            .or_else(|| self.pathing.as_ref().map(|work| work.scope_depth))
            .or_else(|| self.decoding.as_ref().map(|work| work.scope_depth))
            .unwrap_or_else(|| self.work.scope_depth())
    }
}

/// Sole owner of one resumable WHNF request while the reflection machine is
/// decoding an effect. Unlike ordinary [`MachineWork`], this state is never
/// cloned to preserve a retry checkpoint.
struct EffectDecodeWork<S: TaskSpecialization> {
    operation: EffectDecodeOperation<S::Request>,
    branch: Branch<S>,
    scope_depth: usize,
}

enum EffectDecodeOperation<R> {
    Whnf {
        computation: WhnfComputation,
        purpose: EffectDecodePurpose,
    },
    Request(RequestDecodeWork<R>),
}

#[derive(Clone, Copy)]
enum EffectDecodePurpose {
    EffectObject,
    Function,
    RequestApplication,
    AppliedEffect,
}

impl<S: TaskSpecialization> EffectDecodeWork<S> {
    fn new(branch: Branch<S>, scope_depth: usize) -> Self {
        Self::from_root(
            branch.effect.clone(),
            EffectDecodePurpose::EffectObject,
            branch,
            scope_depth,
        )
    }

    fn from_root(
        value: RuntimeValueRoot,
        purpose: EffectDecodePurpose,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self {
            operation: EffectDecodeOperation::Whnf {
                computation: WhnfComputation::from_root(value),
                purpose,
            },
            branch,
            scope_depth,
        }
    }

    fn request(value: RuntimeValueRoot, branch: Branch<S>, scope_depth: usize) -> Self {
        Self {
            operation: EffectDecodeOperation::Request(RequestDecodeWork::new(value)),
            branch,
            scope_depth,
        }
    }

    fn application(
        poll_context: &EvaluationPollContext,
        context: &EvalContext,
        function: RuntimeValueRoot,
        arguments: Vec<RuntimeValueRoot>,
        purpose: EffectDecodePurpose,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        debug_assert!(!arguments.is_empty());
        let computation = poll_context.evaluate(context, |evaluator| {
            evaluator.with_value_access(|access| {
                let function = access.clone_root(&function);
                let arguments = arguments
                    .iter()
                    .map(|argument| access.clone_root(argument))
                    .collect::<Vec<_>>();
                WhnfComputation::from_application_checkpoint_in(&access, function, &arguments, None)
            })
        });
        Self {
            operation: EffectDecodeOperation::Whnf {
                computation,
                purpose,
            },
            branch,
            scope_depth,
        }
    }

    fn contextualize(
        &self,
        halt: TaskHalt,
        poll_context: &EvaluationPollContext,
        context: &EvalContext,
    ) -> TaskHalt {
        let stage = match &self.operation {
            EffectDecodeOperation::Whnf {
                purpose: EffectDecodePurpose::EffectObject,
                ..
            } => return halt,
            EffectDecodeOperation::Whnf {
                purpose: EffectDecodePurpose::Function,
                ..
            } => "function",
            EffectDecodeOperation::Whnf {
                purpose: EffectDecodePurpose::RequestApplication,
                computation,
            } => {
                if computation.application_frame_pending() {
                    "application"
                } else {
                    "request"
                }
            }
            EffectDecodeOperation::Whnf {
                purpose: EffectDecodePurpose::AppliedEffect,
                ..
            } => return halt,
            EffectDecodeOperation::Request(_) => "request",
        };
        poll_context.evaluate(context, |evaluator| {
            evaluator.with_value_access(|access| {
                halt.with_core_context_in(
                    access.values(),
                    effect_dispatch_context(access.values(), stage),
                )
            })
        })
    }
}

enum EffectDecodeStep<S: TaskSpecialization> {
    Continue(EffectDecodeWork<S>),
    Complete(MachineWork<S>),
    Blocked(EffectDecodeWork<S>, WorkDependency),
    BudgetExhausted(EffectDecodeWork<S>),
    Failed(EffectDecodeWork<S>, TaskHalt),
}

/// Owns one scalar WHNF demand whose completion changes reflection control.
///
/// This state is kept beside effect decoding because its completed value is
/// delivered to an existing continuation or terminal intent rather than
/// interpreted as another effect request.
struct ScalarDemandWork<S: TaskSpecialization> {
    computation: WhnfComputation,
    purpose: ScalarDemandPurpose,
    branch: Branch<S>,
    scope_depth: usize,
}

enum ScalarDemandPurpose {
    ApplyContinuation {
        argument: RuntimeValueRoot,
        fused: bool,
    },
    RequireUnit,
    AssertUnit,
    RestoreScopedValue {
        scoped_value: RuntimeValueRoot,
    },
    ExitError,
}

impl<S: TaskSpecialization> ScalarDemandWork<S> {
    fn new(
        value: RuntimeValueRoot,
        purpose: ScalarDemandPurpose,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self {
            computation: WhnfComputation::from_root(value),
            purpose,
            branch,
            scope_depth,
        }
    }
}

enum ScalarDemandStep<S: TaskSpecialization> {
    Complete(MachineStep<S>),
    Blocked(ScalarDemandWork<S>, WorkDependency),
    BudgetExhausted(ScalarDemandWork<S>),
    Failed(ScalarDemandWork<S>, TaskHalt),
}

/// Sole owner of specialization-defined request progress.
///
/// The request state is never cloned into a retry checkpoint. A semantic
/// dependency retains its WHNF computation here, while an optimistic
/// transaction retry reconstructs fresh request work from the branch
/// checkpoint.
struct SpecializationWork<S: TaskSpecialization> {
    request: SpecializationRequest<S>,
    input: Option<SpecializationRequestInput>,
    demand: Option<WhnfComputation>,
    branch: Branch<S>,
    scope_depth: usize,
}

/// A specialized request, started by its first poll: starting runs the
/// specialization's code, which shares that poll's suspension.
enum SpecializationRequest<S: TaskSpecialization> {
    Unstarted {
        request: S::Request,
        arguments: Vec<PublicValue>,
    },
    Started(S::RequestWork),
}

impl<S: TaskSpecialization> SpecializationRequest<S> {
    fn started(&mut self, specialization: &S) -> &mut S::RequestWork {
        if let Self::Unstarted { request, arguments } = self {
            let work = specialization.start_request(request.clone(), std::mem::take(arguments));
            *self = Self::Started(work);
        }
        match self {
            Self::Started(work) => work,
            Self::Unstarted { .. } => unreachable!("started above"),
        }
    }
}

impl<S: TaskSpecialization> SpecializationWork<S> {
    fn new(request: SpecializationRequest<S>, branch: Branch<S>, scope_depth: usize) -> Self {
        Self {
            request,
            input: None,
            demand: None,
            branch,
            scope_depth,
        }
    }
}

enum SpecializationStep<S: TaskSpecialization> {
    Continue(Box<SpecializationWork<S>>),
    Complete(Box<MachineWork<S>>),
    Blocked(Box<SpecializationWork<S>>, WorkDependency),
    BudgetExhausted(Box<SpecializationWork<S>>),
    Failed(Box<SpecializationWork<S>>, TaskHalt),
}

/// Owns one reset-stack-dependent transition while its serialized control
/// state is being decoded. Like the other task work owners, it retains the
/// exact branch and scope needed to resume after a yield or dependency.
struct ControlWork<S: TaskSpecialization> {
    operation: ControlOperation<S>,
    branch: Branch<S>,
    scope_depth: usize,
}

enum ControlOperation<S: TaskSpecialization> {
    Key {
        key: eval::KeyConversionMachine,
        stack: ResetStackMachine,
        disposition: KeyedControl,
    },
    Stack {
        key: Key,
        stack: ResetStackMachine,
        disposition: KeyedControl,
    },
    InstallCaptured {
        stack: ResetStackMachine,
        captured: CapturedContinuation,
        value: RuntimeValueRoot,
    },
    StartFixpoint {
        stack: ResetStackMachine,
        root: Arc<FixRoot<S>>,
        choices: Vec<FixChoice>,
    },
    Delivery {
        stack: ResetStackMachine,
        value: RuntimeValueRoot,
    },
    Restore {
        stack: ResetStackMachine,
        outer: Box<Control>,
        value: RuntimeValueRoot,
    },
    Poisoned,
}

enum KeyedControl {
    Reset { operation: RuntimeValueRoot },
    Shift { function: RuntimeValueRoot },
}

impl<S: TaskSpecialization> ControlWork<S> {
    fn reset(
        key: RuntimeValueRoot,
        stack: RuntimeValueRoot,
        operation: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self {
            operation: ControlOperation::Key {
                key: eval::KeyConversionMachine::new(key, None),
                stack: ResetStackMachine::new(stack),
                disposition: KeyedControl::Reset { operation },
            },
            branch,
            scope_depth,
        }
    }

    fn shift(
        key: RuntimeValueRoot,
        stack: RuntimeValueRoot,
        function: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self {
            operation: ControlOperation::Key {
                key: eval::KeyConversionMachine::new(key, None),
                stack: ResetStackMachine::new(stack),
                disposition: KeyedControl::Shift { function },
            },
            branch,
            scope_depth,
        }
    }

    fn poisoned(branch: Branch<S>, scope_depth: usize) -> Self {
        Self {
            operation: ControlOperation::Poisoned,
            branch,
            scope_depth,
        }
    }

    fn install_captured(
        stack: RuntimeValueRoot,
        captured: CapturedContinuation,
        value: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self {
            operation: ControlOperation::InstallCaptured {
                stack: ResetStackMachine::new(stack),
                captured,
                value,
            },
            branch,
            scope_depth,
        }
    }

    fn start_fixpoint(
        stack: RuntimeValueRoot,
        root: Arc<FixRoot<S>>,
        choices: Vec<FixChoice>,
    ) -> Self {
        Self::start_fixpoint_with_restarts(stack, root, choices, Vec::new())
    }

    fn start_fixpoint_with_restarts(
        stack: RuntimeValueRoot,
        root: Arc<FixRoot<S>>,
        choices: Vec<FixChoice>,
        inherited_restarts: Vec<FixRestart<S>>,
    ) -> Self {
        let mut branch = root.entry.clone();
        branch.fix_restarts = inherited_restarts;
        Self {
            operation: ControlOperation::StartFixpoint {
                stack: ResetStackMachine::new(stack),
                root: root.clone(),
                choices,
            },
            branch,
            scope_depth: root.scope_depth,
        }
    }

    fn delivery(
        stack: RuntimeValueRoot,
        value: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self {
            operation: ControlOperation::Delivery {
                stack: ResetStackMachine::new(stack),
                value,
            },
            branch,
            scope_depth,
        }
    }
}

enum ControlStep<S: TaskSpecialization> {
    Continue(ControlWork<S>),
    Complete(MachineWork<S>),
    Blocked(ControlWork<S>, WorkDependency),
    BudgetExhausted(ControlWork<S>),
    Failed(ControlWork<S>, TaskHalt),
}

struct StatePathWork<S: TaskSpecialization> {
    operation: StatePathOperation,
    branch: Branch<S>,
    scope_depth: usize,
}

enum StatePathOperation {
    Keys {
        machine: Box<eval::KeyListMachine>,
        after: Option<StatePathAfterKeys>,
    },
    Get(ValuePathMachine),
    SetBase {
        computation: WhnfComputation,
        path: Arc<[Key]>,
        value: RuntimeValueRoot,
    },
    SetUpdate(WhnfComputation),
    Poisoned,
}

enum StatePathAfterKeys {
    Get {
        state: RuntimeValueRoot,
    },
    Set {
        state: RuntimeValueRoot,
        value: RuntimeValueRoot,
    },
    Store(StorePathOperation),
}

enum StorePathOperation {
    HeapGet,
    HeapSet(RuntimeValueRoot),
    HeapRewrite(RuntimeValueRoot),
    HeapTryGet,
    HeapDel,
    VolumeGet(VolumeTarget),
    VolumeSet(VolumeTarget, RuntimeValueRoot),
    VolumeRewrite(VolumeTarget, RuntimeValueRoot),
    VolumeTryGet(VolumeTarget),
    VolumeDel(VolumeTarget),
}

impl<S: TaskSpecialization> StatePathWork<S> {
    fn keys(
        path: RuntimeValueRoot,
        after: StatePathAfterKeys,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self {
            operation: StatePathOperation::Keys {
                machine: Box::new(eval::KeyListMachine::unowned(path)),
                after: Some(after),
            },
            branch,
            scope_depth,
        }
    }

    fn get(path: RuntimeValueRoot, branch: Branch<S>, scope_depth: usize) -> Self {
        let state = branch.state.clone();
        Self::keys(path, StatePathAfterKeys::Get { state }, branch, scope_depth)
    }

    fn set(
        path: RuntimeValueRoot,
        value: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        let state = branch.state.clone();
        Self::keys(
            path,
            StatePathAfterKeys::Set { state, value },
            branch,
            scope_depth,
        )
    }

    /// A store operation at `path`, once its keys are converted.
    fn store(
        path: RuntimeValueRoot,
        operation: StorePathOperation,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self::keys(
            path,
            StatePathAfterKeys::Store(operation),
            branch,
            scope_depth,
        )
    }

    /// `.del Path` on the user's state: `update`, the state without `Path`,
    /// pruning the dicts the removal empties, becomes the state.
    fn del(update: RuntimeValueRoot, branch: Branch<S>, scope_depth: usize) -> Self {
        Self {
            operation: StatePathOperation::SetUpdate(WhnfComputation::from_root(update)),
            branch,
            scope_depth,
        }
    }

    fn heap_get(path: RuntimeValueRoot, branch: Branch<S>, scope_depth: usize) -> Self {
        Self::keys(
            path,
            StatePathAfterKeys::Store(StorePathOperation::HeapGet),
            branch,
            scope_depth,
        )
    }

    fn heap_set(
        path: RuntimeValueRoot,
        value: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self::keys(
            path,
            StatePathAfterKeys::Store(StorePathOperation::HeapSet(value)),
            branch,
            scope_depth,
        )
    }

    fn heap_rewrite(
        path: RuntimeValueRoot,
        updater: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self::keys(
            path,
            StatePathAfterKeys::Store(StorePathOperation::HeapRewrite(updater)),
            branch,
            scope_depth,
        )
    }

    fn volume_get(
        volume: VolumeTarget,
        path: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self::keys(
            path,
            StatePathAfterKeys::Store(StorePathOperation::VolumeGet(volume)),
            branch,
            scope_depth,
        )
    }

    fn volume_set(
        volume: VolumeTarget,
        path: RuntimeValueRoot,
        value: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self::keys(
            path,
            StatePathAfterKeys::Store(StorePathOperation::VolumeSet(volume, value)),
            branch,
            scope_depth,
        )
    }

    fn volume_rewrite(
        volume: VolumeTarget,
        path: RuntimeValueRoot,
        updater: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self::keys(
            path,
            StatePathAfterKeys::Store(StorePathOperation::VolumeRewrite(volume, updater)),
            branch,
            scope_depth,
        )
    }

    fn poisoned(branch: Branch<S>, scope_depth: usize) -> Self {
        Self {
            operation: StatePathOperation::Poisoned,
            branch,
            scope_depth,
        }
    }
}

enum StatePathStep<S: TaskSpecialization> {
    Continue(StatePathWork<S>),
    Complete(MachineWork<S>),
    Blocked(StatePathWork<S>, WorkDependency),
    BudgetExhausted(StatePathWork<S>),
    Failed(StatePathWork<S>, TaskHalt),
}

struct ValuePathMachine {
    path: Arc<[Key]>,
    next: usize,
    current: RuntimeValueRoot,
    demand: Option<WhnfComputation>,
}

impl ValuePathMachine {
    fn new(current: RuntimeValueRoot, path: Arc<[Key]>) -> Self {
        Self {
            path,
            next: 0,
            current,
            demand: None,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ValuePathPoll {
        if self.next == self.path.len() {
            return ValuePathPoll::Ready(self.current.clone());
        }
        let demand = self
            .demand
            .get_or_insert_with(|| WhnfComputation::from_root(self.current.clone()));
        let current = match poll_whnf_computation(demand, poll_context, context, step_budget) {
            WhnfOwnerPoll::Ready(current) => current,
            WhnfOwnerPoll::Pending(dependency) => return ValuePathPoll::Pending(dependency),
            WhnfOwnerPoll::BudgetExhausted => return ValuePathPoll::BudgetExhausted,
            WhnfOwnerPoll::Failed(failure) => {
                return ValuePathPoll::Failed(TaskHalt::rooted_failure(failure));
            }
        };
        let key = &self.path[self.next];
        let walked = &self.path[..=self.next];
        let selected = poll_context.evaluate(context, |evaluator| {
            evaluator.project_root(&current, |access, current| {
                let Value::Dict(dict) = current else {
                    return Err(TaskHalt::new("state path traverses a non-dictionary value"));
                };
                // What is undefined reads as an error value, which only an
                // observer of the value meets (`dict-empty-is-a-value`).
                Ok(match dict.get(key) {
                    Some(value) => (
                        access.root_value(access.values().duplicate_value(value)),
                        true,
                    ),
                    None => (
                        access.root_value(eval::undefined_path_value(access.values(), walked)),
                        false,
                    ),
                })
            })
        });
        match selected {
            Ok((selected, defined)) => {
                self.current = selected;
                self.next = if defined {
                    self.next + 1
                } else {
                    self.path.len()
                };
                self.demand = None;
                ValuePathPoll::Continue
            }
            Err(error) => ValuePathPoll::Failed(error),
        }
    }
}

enum ValuePathPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    Continue,
    BudgetExhausted,
    Failed(TaskHalt),
}

#[derive(Clone)]
enum MachineWork<S: TaskSpecialization> {
    Drive {
        branch: Branch<S>,
        scope_depth: usize,
    },
    Deliver {
        value: RuntimeValueRoot,
        branch: Branch<S>,
        scope_depth: usize,
    },
    Apply {
        function: RuntimeValueRoot,
        arguments: Vec<RuntimeValueRoot>,
        branch: Branch<S>,
        scope_depth: usize,
    },
    Interpret {
        request: Request<S::Request>,
        branch: Branch<S>,
        scope_depth: usize,
    },
    Outcome {
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    },
}

impl<S: TaskSpecialization> MachineWork<S> {
    fn deliver(
        access: &RuntimeValueAccess<'_>,
        value: Value,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        let value = branch.root_value(access, value);
        Self::Deliver {
            value,
            branch,
            scope_depth,
        }
    }

    fn deliver_root(value: RuntimeValueRoot, branch: Branch<S>, scope_depth: usize) -> Self {
        debug_assert_eq!(value.runtime_id(), branch.effect.runtime_id());
        Self::Deliver {
            value,
            branch,
            scope_depth,
        }
    }

    fn deliver_unit(
        access: &RuntimeValueAccess<'_>,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        Self::deliver(access, access.unit(), branch, scope_depth)
    }

    #[cfg(test)]
    fn apply(
        access: &RuntimeValueAccess<'_>,
        function: Value,
        arguments: Vec<Value>,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        let function = branch.root_value(access, function);
        let arguments = arguments
            .into_iter()
            .map(|argument| branch.root_value(access, argument))
            .collect();
        Self::apply_roots(function, arguments, branch, scope_depth)
    }

    fn apply_roots(
        function: RuntimeValueRoot,
        arguments: Vec<RuntimeValueRoot>,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        debug_assert_eq!(function.runtime_id(), branch.effect.runtime_id());
        debug_assert!(
            arguments
                .iter()
                .all(|argument| argument.runtime_id() == branch.effect.runtime_id())
        );
        Self::Apply {
            function,
            arguments,
            branch,
            scope_depth,
        }
    }

    fn branch(&self) -> Option<&Branch<S>> {
        match self {
            Self::Drive { branch, .. }
            | Self::Deliver { branch, .. }
            | Self::Apply { branch, .. }
            | Self::Interpret { branch, .. } => Some(branch),
            Self::Outcome { outcome, .. } => outcome.branch(),
        }
    }

    fn branch_mut(&mut self) -> Option<&mut Branch<S>> {
        match self {
            Self::Drive { branch, .. }
            | Self::Deliver { branch, .. }
            | Self::Apply { branch, .. }
            | Self::Interpret { branch, .. } => Some(branch),
            Self::Outcome { outcome, .. } => outcome.branch_mut(),
        }
    }

    fn scope_depth(&self) -> usize {
        match self {
            Self::Drive { scope_depth, .. }
            | Self::Deliver { scope_depth, .. }
            | Self::Apply { scope_depth, .. }
            | Self::Interpret { scope_depth, .. }
            | Self::Outcome { scope_depth, .. } => *scope_depth,
        }
    }
}

#[derive(Clone)]
enum BranchOutcome<S: TaskSpecialization> {
    Complete(RuntimeValueRoot, Branch<S>),
    Fork(Box<Branch<S>>, Box<Branch<S>>),
    Fail(Branch<S>),
    Retry(Branch<S>),
    Cancelled,
}

impl<S: TaskSpecialization> BranchOutcome<S> {
    #[cfg(test)]
    fn complete(values: &CoreValueFactory, value: Value, branch: Branch<S>) -> Self {
        let value = values.with_boundary_access(|access| branch.root_value(&access, value));
        Self::Complete(value, branch)
    }

    fn branch(&self) -> Option<&Branch<S>> {
        match self {
            Self::Complete(_, branch) | Self::Fail(branch) | Self::Retry(branch) => Some(branch),
            Self::Fork(left, _) => Some(left),
            Self::Cancelled => None,
        }
    }

    fn branch_mut(&mut self) -> Option<&mut Branch<S>> {
        match self {
            Self::Complete(_, branch) | Self::Fail(branch) | Self::Retry(branch) => Some(branch),
            Self::Fork(left, _) => Some(left),
            Self::Cancelled => None,
        }
    }
}

#[derive(Clone)]
struct CutFrame<S: TaskSpecialization> {
    operation: RuntimeValueRoot,
    outer: Branch<S>,
    outer_sequence: Vec<Continuation>,
    parent_scope_depth: usize,
    scope_depth: usize,
    owns_transaction: bool,
    alternatives: Vec<Branch<S>>,
    /// The second operand of a choice made outside every cut, whose first
    /// operand this cut is: it runs in the cut's place should the cut fail.
    fallback: Option<Branch<S>>,
    retry: Option<Branch<S>>,
    observed_failure: bool,
    /// A `.try`'s frame, which returns its operation's outcome as data
    /// rather than retrying it (`api-refl-try`).
    trying: bool,
}

/// What a `.try` answers with, other than its operation's value
/// (`api-refl-try`).
enum TryOutcome {
    /// An explicit failure, with no alternative left: the atom `'fail`,
    /// which carries nothing, as the failure is intentional.
    Fail,
    /// A wait on what is outside the `.try`'s control: a pending promise.
    Wait,
    /// A commit that conflicted with another's.
    Conflict,
    /// An evaluation error.
    Err(TaskHalt),
}

/// What a `.try`'s abandoned operation leaves recorded in the transaction
/// around it: whether it observed shared state, and what it waited on.
#[derive(Default)]
struct TryTrace {
    observed: bool,
    /// The store's promise a `.try` answered `wait` on.
    waited: Option<PromiseId>,
}

/// `seq value (.r value)`, the operation `.eval value` tries.
/// `.try_get`'s work (`dict-empty-is-a-value`): hold the value at `path` in
/// `root`, then run `dict.defines`, which fails where `path` is undefined
/// and otherwise returns unit, on which the held value is delivered.
fn try_get_work<S: TaskSpecialization>(
    access: &RuntimeValueAccess<'_>,
    root: &Value,
    path: Value,
    mut branch: Branch<S>,
    scope_depth: usize,
) -> MachineWork<S> {
    let lookup = branch.root_value(
        access,
        Value::builtin_call_in(
            access,
            Builtin::DictLookup,
            vec![access.duplicate_value(&path), access.duplicate_value(root)],
        ),
    );
    let defines = branch.root_value(
        access,
        Value::builtin_call_in(
            access,
            Builtin::DictDefines,
            vec![path, access.duplicate_value(root)],
        ),
    );
    branch
        .control
        .sequence
        .push(Continuation::RestoreScopedValue(lookup));
    branch.set_effect_root(defines);
    MachineWork::Drive {
        branch,
        scope_depth,
    }
}

/// `keys` as a path value.
fn key_path_in(access: &RuntimeValueAccess<'_>, keys: &[Key]) -> Value {
    Value::List(List::from_values(
        keys.iter().map(|key| access.value_from_key(key)).collect(),
    ))
}

fn evaluation_effect_in(access: &RuntimeValueAccess<'_>, value: Value) -> Value {
    let returned = Value::Dict(Dict::new_sync().insert(
        keys::EFF.clone(),
        Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::EffectCall,
            arguments: Arc::from([
                Value::Atom(Atom::from_key(&Key::binary_from_text("r"))),
                Value::List(List::from_values(vec![access.duplicate_value(&value)])),
            ]),
        }),
    ));
    Value::builtin_call_in(access, Builtin::Seq, vec![value, returned])
}

/// A `.try`'s answer, `{key: payload}`.
fn try_value_in(_access: &RuntimeValueAccess<'_>, key: &Key, payload: Value) -> Value {
    Value::Dict(Dict::new_sync().insert(key.clone(), payload))
}

fn try_outcome_in(access: &RuntimeValueAccess<'_>, outcome: TryOutcome) -> Value {
    let diagnostic = |halt: TaskHalt| {
        halt.diagnostic_in(access)
            .expect("a `.try` reports failures, never panics")
    };
    match outcome {
        TryOutcome::Fail => Value::Atom(Atom::from_key(&Key::binary_from_text("fail"))),
        TryOutcome::Wait => try_value_in(
            access,
            &keys::WAIT,
            diagnostic(TaskHalt::new(
                "`.try` waits on a promise no one has resolved yet",
            )),
        ),
        TryOutcome::Conflict => try_value_in(
            access,
            &keys::CONFLICT,
            diagnostic(TaskHalt::new(
                "`.try`'s transaction conflicted with another's commit",
            )),
        ),
        TryOutcome::Err(halt) => try_value_in(access, &keys::ERR, diagnostic(halt)),
    }
}

impl<S: TaskSpecialization> CutFrame<S> {
    fn next_alternative(&mut self) -> MachineWork<S> {
        MachineWork::Drive {
            branch: self
                .alternatives
                .pop()
                .expect("cut attempt must have another alternative"),
            scope_depth: self.scope_depth,
        }
    }
}

#[cfg(test)]
const EFFECT_FUSION_BUDGET: usize = 32;

// This value is short-lived on the Rust stack. Boxing `Continue` would add an
// allocation to every cooperative machine transition merely to shrink the two
// uncommon terminal variants.
#[allow(clippy::large_enum_variant)]
enum MachineStep<S: TaskSpecialization> {
    Continue(MachineWork<S>),
    Decode(EffectDecodeWork<S>),
    Demand(ScalarDemandWork<S>),
    StatePath(Box<StatePathWork<S>>),
    Control(Box<ControlWork<S>>),
    Specialize(Box<SpecializationWork<S>>),
    Blocked(BlockedExecution<S>),
    Exit(ExitIntent),
    Terminal(TaskTerminal),
}

struct BlockedExecution<S: TaskSpecialization> {
    reason: BlockReason,
    retry: Option<RetryWake<S>>,
}

impl<S: TaskSpecialization> BlockedExecution<S> {
    fn waiting_on(dependency: WorkDependency, retry: Option<RetryWake<S>>) -> Self {
        Self {
            reason: BlockReason::WaitingOn(dependency),
            retry,
        }
    }

    fn exhausted(retry: RetryWake<S>) -> Self {
        Self {
            reason: BlockReason::Exhausted,
            retry: Some(retry),
        }
    }

    fn evaluation_error(error: TaskHalt, retry: RetryWake<S>, values: &CoreValueFactory) -> Self {
        Self {
            reason: BlockReason::EvaluationError(error.root_for_values(values)),
            retry: Some(retry),
        }
    }

    fn dependency(&self) -> Option<WorkDependency> {
        match &self.reason {
            BlockReason::WaitingOn(dependency) => Some(dependency.clone()),
            BlockReason::Exhausted | BlockReason::EvaluationError(_) => None,
        }
    }

    fn observed_generation(&self) -> Option<u64> {
        self.retry.as_ref().map(|retry| retry.observed_generation)
    }

    fn error(&self) -> Option<crate::runtime::RuntimeFailureRoot> {
        match &self.reason {
            BlockReason::EvaluationError(error) => Some(
                error
                    .failure_root()
                    .expect("evaluation-error blocks retain permanent failure roots")
                    .clone(),
            ),
            BlockReason::WaitingOn(_) | BlockReason::Exhausted => None,
        }
    }
}

enum BlockReason {
    WaitingOn(WorkDependency),
    Exhausted,
    EvaluationError(TaskHalt),
}

struct RetryWake<S: TaskSpecialization> {
    observed_generation: u64,
    validation: Option<Transaction<S>>,
    action: WakeAction<S>,
}

enum BlockedRetryPoll {
    Stable,
    Restarted,
    Terminal(EffectTaskPoll),
}

enum WakeAction<S: TaskSpecialization> {
    ReplaceWork(Box<MachineWork<S>>),
    RestartCut(usize),
    RestartSearch,
}

pub(super) struct TaskBlock {
    pub(super) dependency: Option<WorkDependency>,
    pub(super) observed_generation: Option<u64>,
    pub(super) error: Option<crate::runtime::RuntimeFailureRoot>,
}

#[derive(Clone)]
pub(super) struct TaskExitBlock {
    intent: ExitIntent,
    observed_generation: Option<u64>,
}

struct TaskExitState<S: TaskSpecialization> {
    poll: TaskExitBlock,
    restart: Option<RetryWake<S>>,
}

/// What a task whose host runs its cuts tells the host
/// (`alt-cuts-its-first-operand`). The host keeps the effects of the task's
/// requests, which a failed alternative must undo, so the task reports
/// where its outermost cut begins and ends, and where it resumes a branch
/// set aside at an earlier version of the host's state: the count of
/// requests the host had answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostControl {
    /// The outermost cut began, at this version.
    CutBegin(u64),
    /// The outermost cut succeeded: the host commits it, then confirms the
    /// commit or, where the cut `retries`, restarts it. A `.try` does not
    /// retry: a conflict is its outcome, and the host ends its transaction.
    CutCommit { retries: bool },
    /// The outermost cut failed: the host returns to the version it began
    /// at.
    CutAbort(u64),
    /// The task resumes a branch set aside at this version.
    Restore(u64),
}

/// How a task whose host runs its cuts ended.
pub(crate) enum HostedResult {
    /// With its value, which the terminal holds, and this state.
    Complete(PublicValue),
    /// With a failure that nothing handled.
    Backtracked,
}

/// What a task whose host runs its cuts keeps for the host.
struct HostedCuts<S: TaskSpecialization> {
    /// The count of requests the host has answered.
    version: u64,
    /// Whether a branch was set aside since the host last answered, so
    /// the host keeps a way back to its state before it answers again.
    set_aside: bool,
    /// The event the last step raised, which the task pauses for.
    pending: Option<HostControl>,
    /// The outermost cut, while the host commits it.
    committing: Option<CutFrame<S>>,
    result: Option<HostedResult>,
}

impl<S: TaskSpecialization> Default for HostedCuts<S> {
    fn default() -> Self {
        Self {
            version: 0,
            set_aside: false,
            pending: None,
            committing: None,
            result: None,
        }
    }
}

pub(super) enum EffectTaskPoll {
    BudgetExhausted,
    Blocked(TaskBlock),
    Exit(TaskExitBlock),
    /// Where the host runs the cuts, paused for the host to handle this.
    Paused(HostControl),
    Complete(PublicValue),
    Failed(TaskHalt),
    Cancelled,
}

#[derive(Clone)]
pub(super) enum TaskTerminal {
    Complete(PublicValue),
    Failed(TaskHalt),
    Cancelled,
}

impl TaskTerminal {
    fn poll(&self) -> EffectTaskPoll {
        match self {
            Self::Complete(value) => EffectTaskPoll::Complete(value.clone()),
            Self::Failed(error) => EffectTaskPoll::Failed(error.clone()),
            Self::Cancelled => EffectTaskPoll::Cancelled,
        }
    }
}

#[derive(Clone)]
struct RetryCheckpoint<S: TaskSpecialization> {
    generation: Option<u64>,
    branch: Box<Branch<S>>,
}

#[derive(Clone)]
struct FixRoot<S: TaskSpecialization> {
    function: RuntimeValueRoot,
    entry: Branch<S>,
    scope_depth: usize,
}

#[derive(Clone)]
struct ActiveFix<S: TaskSpecialization> {
    root: Arc<FixRoot<S>>,
    choices: Vec<FixChoice>,
    next_choice: usize,
    handle: crate::core::ManagedPromiseRoot,
}

#[derive(Clone)]
struct FixRestart<S: TaskSpecialization> {
    root: Arc<FixRoot<S>>,
    choices: Vec<FixChoice>,
    inherited_restarts: Vec<FixRestart<S>>,
}

#[derive(Clone, Copy)]
enum FixChoice {
    Left,
    Right,
}

#[derive(Clone, Default)]
struct Control {
    sequence: Vec<Continuation>,
    delimiters: Vec<Delimiter>,
}

#[derive(Clone)]
enum Continuation {
    Glam(RuntimeValueRoot),
    RequireUnit,
    AssertUnit(RuntimeValueRoot),
    Fix(crate::core::ManagedPromiseRoot),
    CloseScope(RuntimeValueRoot),
    RestoreScopedValue(RuntimeValueRoot),
}

#[derive(Clone)]
enum Delimiter {
    Resume {
        outer_sequence: Vec<Continuation>,
        scope_depth: usize,
        order: usize,
    },
    Restore {
        outer: Box<Control>,
        reset_stack: RuntimeValueRoot,
        scope_depth: usize,
        order: usize,
    },
}

impl Delimiter {
    fn scope_depth(&self) -> usize {
        match self {
            Self::Resume { scope_depth, .. } | Self::Restore { scope_depth, .. } => *scope_depth,
        }
    }

    fn order(&self) -> usize {
        match self {
            Self::Resume { order, .. } | Self::Restore { order, .. } => *order,
        }
    }

    fn rebase(&mut self, scope_depth: usize, order: usize) {
        match self {
            Self::Resume {
                scope_depth: depth,
                order: position,
                ..
            }
            | Self::Restore {
                scope_depth: depth,
                order: position,
                ..
            } => {
                *depth = scope_depth;
                *position = order;
            }
        }
    }
}

#[derive(Clone)]
struct CapturedContinuation {
    sequence: Vec<Continuation>,
    delimiters: Vec<Delimiter>,
    reset_frames: Vec<ResetFrame>,
}

#[derive(Clone)]
struct ResetFrame {
    // Reset frames are encoded as ordinary Values under continuation_state.
    // scope_depth and order preserve nesting with the handler's temporary
    // cut/resume/fix control without creating a second authoritative stack.
    key: Key,
    continuation: RuntimeValueRoot,
    scope_depth: usize,
    order: usize,
}

enum CapturedLayer {
    Reset(ResetFrame),
    Delimiter(Delimiter),
}

impl CapturedLayer {
    fn order(&self) -> usize {
        match self {
            Self::Reset(frame) => frame.order,
            Self::Delimiter(delimiter) => delimiter.order(),
        }
    }
}

#[derive(Clone)]
enum Request<R, V = RuntimeValueRoot> {
    Return(V),
    Seq(V, V),
    Alt(V, V),
    Fail,
    Cut(V),
    Try(V),
    Eval(V),
    Fix(V),
    Get(V),
    Set(V, V),
    HeapGet(V),
    HeapSet(V, V),
    HeapRewrite(V, V),
    HeapVolume,
    /// The value at a path where defined, a failure where not, and a
    /// removal (`dict-empty-is-a-value`).
    TryGet(V),
    Del(V),
    HeapTryGet(V),
    HeapDel(V),
    VolumeTryGet(VolumeTarget<V>, V),
    VolumeDel(VolumeTarget<V>, V),
    VolumeGet(VolumeTarget<V>, V),
    VolumeSet(VolumeTarget<V>, V, V),
    VolumeRewrite(VolumeTarget<V>, V, V),
    VolumeNew(V),
    PromiseNew,
    PromiseStatus(ManagedResolverRoot),
    PromiseFulfill(ManagedResolverRoot, V),
    PromiseBreak(ManagedResolverRoot, V),
    Reset(V, V),
    Shift(V, V),
    Resume(EvaluationTaskId, u64, V),
    ExitSuccess,
    ExitError(V),
    Specialized(R, Vec<V>),
}

struct SpecializedRequest<R> {
    tag: Key,
    arity: usize,
    request: R,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VolumeOperation {
    Get,
    Set,
    Rewrite,
    TryGet,
    Del,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromiseOperation {
    Status,
    Fulfill,
    Break,
}

/// An operation whose first argument is a handle, which decoding evaluates
/// (`requests-travel-on-ports`): a volume's, or a promise's resolver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandleOperation {
    Volume(VolumeOperation),
    Promise(PromiseOperation),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VolumeRequestIdentity {
    volume: VolumeId,
    operation: VolumeOperation,
}

/// The volume a request names (`api-volume-cells`).
#[derive(Clone)]
enum VolumeTarget<V = RuntimeValueRoot> {
    /// A host's protected volume, by its ID, which the store keeps until
    /// the host revokes it.
    Kept(VolumeId),
    /// A volume's handle, as the request decoded it, which keeps the volume
    /// until the request completes.
    Handle(V),
}

impl VolumeTarget {
    /// The volume's ID, for diagnostics.
    fn id_in(&self, access: &RuntimeValueAccess<'_>) -> VolumeId {
        match self {
            Self::Kept(volume) => *volume,
            Self::Handle(handle) => handle_volume(access, &handle.clone_core_with(access))
                .expect("decoding checked the volume's handle"),
        }
    }

    /// A handle's volume, viewed.
    fn handle_cell<'access>(
        access: &'access RuntimeValueAccess<'_>,
        handle: &RuntimeValueRoot,
    ) -> VolumeCellView<'access> {
        access
            .volume_cell(&handle.clone_core_with(access))
            .expect("decoding checked the volume's handle")
    }

    /// The volume's cell, viewed: the handle's, or for a host's volume, the
    /// one the store kept in `snapshot`, unless the host revoked it.
    fn cell_in<'access>(
        &self,
        access: &'access RuntimeValueAccess<'_>,
        snapshot: &StoreSnapshot,
    ) -> Option<VolumeCellView<'access>> {
        match self {
            Self::Kept(volume) => snapshot.kept_volume_in(access, *volume),
            Self::Handle(handle) => Some(Self::handle_cell(access, handle)),
        }
    }

    /// Records a read at `path` in the volume within `journal`'s
    /// transaction.
    fn observe_read(
        &self,
        journal: &mut StoreJournal,
        access: &RuntimeValueAccess<'_>,
        path: &[Key],
    ) -> bool {
        match self {
            Self::Kept(volume) => journal.observe_kept_volume_read(*volume, path),
            Self::Handle(handle) => {
                journal.observe_volume_read(Self::handle_cell(access, handle), path)
            }
        }
    }

    /// Reads the volume within `journal`'s transaction.
    fn read_in<'access>(
        &self,
        journal: &mut StoreJournal,
        access: &'access RuntimeValueAccess<'_>,
    ) -> Option<VolumeContentsView<'access>> {
        let volume = self.cell_in(access, journal.snapshot())?;
        journal.read_volume_in(access, volume)
    }

    /// Reads the volume as `snapshot` holds it.
    fn read_snapshot_in<'access>(
        &self,
        snapshot: &StoreSnapshot,
        access: &'access RuntimeValueAccess<'_>,
    ) -> Option<VolumeContentsView<'access>> {
        snapshot.read_volume_in(access, self.cell_in(access, snapshot)?)
    }

    /// Writes `value` at `path` in the volume within `journal`'s
    /// transaction: a host's volume by its ID, blindly, so a write to one
    /// the host revoked fails at the commit.
    fn write_in(
        &self,
        journal: &mut StoreJournal,
        access: &RuntimeValueAccess<'_>,
        path: Vec<Key>,
        value: PublicValue,
    ) {
        match self {
            Self::Kept(volume) => journal.write_kept_volume_in(access, *volume, path, value),
            Self::Handle(handle) => {
                journal.write_volume_in(access, Self::handle_cell(access, handle), path, value)
            }
        }
    }

    /// As [`Self::write_in`], for a removal.
    fn remove_in(
        &self,
        journal: &mut StoreJournal,
        access: &RuntimeValueAccess<'_>,
        path: Vec<Key>,
    ) {
        match self {
            Self::Kept(volume) => journal.remove_kept_volume_in(access, *volume, path),
            Self::Handle(handle) => {
                journal.remove_volume_in(access, Self::handle_cell(access, handle), path)
            }
        }
    }

    /// As [`Self::write_in`], for a rewrite.
    fn rewrite_in(
        &self,
        journal: &mut StoreJournal,
        access: &RuntimeValueAccess<'_>,
        path: Vec<Key>,
        updater: PublicValue,
    ) {
        match self {
            Self::Kept(volume) => journal.rewrite_kept_volume_in(access, *volume, path, updater),
            Self::Handle(handle) => {
                journal.rewrite_volume_in(access, Self::handle_cell(access, handle), path, updater)
            }
        }
    }
}

enum RequestSelection<R> {
    Return,
    Seq,
    Alt,
    Fail,
    Cut,
    Try,
    Eval,
    Fix,
    Get,
    Set,
    HeapGet,
    HeapSet,
    HeapRewrite,
    HeapVolume,
    TryGet,
    Del,
    HeapTryGet,
    HeapDel,
    Volume(VolumeRequestIdentity),
    Handle(HandleOperation),
    VolumeNew,
    PromiseNew,
    Reset,
    Shift,
    Resume,
    ExitSuccess,
    ExitError,
    Specialized { request: R, arity: usize },
}

impl<R> RequestSelection<R> {
    fn arity(&self) -> usize {
        match self {
            Self::Fail | Self::HeapVolume | Self::PromiseNew | Self::ExitSuccess => 0,
            Self::Return
            | Self::Cut
            | Self::Try
            | Self::Eval
            | Self::Fix
            | Self::Get
            | Self::HeapGet
            | Self::TryGet
            | Self::Del
            | Self::HeapTryGet
            | Self::HeapDel
            | Self::ExitError
            | Self::VolumeNew
            | Self::Handle(HandleOperation::Promise(PromiseOperation::Status))
            | Self::Volume(VolumeRequestIdentity {
                operation: VolumeOperation::Get | VolumeOperation::TryGet | VolumeOperation::Del,
                ..
            }) => 1,
            Self::Handle(
                HandleOperation::Volume(
                    VolumeOperation::Get | VolumeOperation::TryGet | VolumeOperation::Del,
                )
                | HandleOperation::Promise(PromiseOperation::Fulfill | PromiseOperation::Break),
            ) => 2,
            Self::Handle(HandleOperation::Volume(
                VolumeOperation::Set | VolumeOperation::Rewrite,
            )) => 3,
            Self::Seq
            | Self::Alt
            | Self::Set
            | Self::HeapSet
            | Self::HeapRewrite
            | Self::Reset
            | Self::Shift
            | Self::Volume(VolumeRequestIdentity {
                operation: VolumeOperation::Set | VolumeOperation::Rewrite,
                ..
            }) => 2,
            Self::Resume => 3,
            Self::Specialized { arity, .. } => *arity,
        }
    }

    fn into_request(self, arguments: Vec<RuntimeValueRoot>) -> Request<R> {
        let mut arguments = arguments.into_iter();
        let mut next = || {
            arguments
                .next()
                .expect("validated request arity must retain its argument")
        };
        match self {
            Self::Return => Request::Return(next()),
            Self::Seq => Request::Seq(next(), next()),
            Self::Alt => Request::Alt(next(), next()),
            Self::Fail => Request::Fail,
            Self::Cut => Request::Cut(next()),
            Self::Try => Request::Try(next()),
            Self::Eval => Request::Eval(next()),
            Self::Fix => Request::Fix(next()),
            Self::Get => Request::Get(next()),
            Self::Set => Request::Set(next(), next()),
            Self::HeapGet => Request::HeapGet(next()),
            Self::HeapSet => Request::HeapSet(next(), next()),
            Self::HeapRewrite => Request::HeapRewrite(next(), next()),
            Self::HeapVolume => Request::HeapVolume,
            Self::TryGet => Request::TryGet(next()),
            Self::Del => Request::Del(next()),
            Self::HeapTryGet => Request::HeapTryGet(next()),
            Self::HeapDel => Request::HeapDel(next()),
            Self::Volume(VolumeRequestIdentity {
                volume,
                operation: VolumeOperation::TryGet,
            }) => Request::VolumeTryGet(VolumeTarget::Kept(volume), next()),
            Self::Volume(VolumeRequestIdentity {
                volume,
                operation: VolumeOperation::Del,
            }) => Request::VolumeDel(VolumeTarget::Kept(volume), next()),
            Self::Volume(VolumeRequestIdentity {
                volume,
                operation: VolumeOperation::Get,
            }) => Request::VolumeGet(VolumeTarget::Kept(volume), next()),
            Self::Volume(VolumeRequestIdentity {
                volume,
                operation: VolumeOperation::Set,
            }) => Request::VolumeSet(VolumeTarget::Kept(volume), next(), next()),
            Self::Volume(VolumeRequestIdentity {
                volume,
                operation: VolumeOperation::Rewrite,
            }) => Request::VolumeRewrite(VolumeTarget::Kept(volume), next(), next()),
            Self::VolumeNew => Request::VolumeNew(next()),
            Self::PromiseNew => Request::PromiseNew,
            Self::Reset => Request::Reset(next(), next()),
            Self::Shift => Request::Shift(next(), next()),
            Self::ExitSuccess => Request::ExitSuccess,
            Self::ExitError => Request::ExitError(next()),
            Self::Specialized { request, .. } => Request::Specialized(request, arguments.collect()),
            Self::Resume => unreachable!("resume IDs require WHNF conversion before dispatch"),
            Self::Handle(_) => {
                unreachable!("handles require WHNF conversion before dispatch")
            }
        }
    }
}

struct RequestDecodeWork<R> {
    state: RequestDecodeState<R>,
}

enum RequestDecodeState<R> {
    Select(RuntimeValueRoot),
    PayloadWhnf {
        selection: RequestSelection<R>,
        demand: WhnfComputation,
    },
    PayloadItems {
        selection: RequestSelection<R>,
        front: eval::ListFrontMachine,
        arguments: Vec<RuntimeValueRoot>,
    },
    ResumeTask {
        continuation: RuntimeValueRoot,
        value: RuntimeValueRoot,
        demand: WhnfComputation,
    },
    ResumeContinuation {
        task: EvaluationTaskId,
        value: RuntimeValueRoot,
        demand: WhnfComputation,
    },
    /// Evaluates an operation's handle, with the operation's other
    /// arguments waiting.
    Handle {
        operation: HandleOperation,
        rest: Vec<RuntimeValueRoot>,
        demand: WhnfComputation,
    },
    Poisoned,
}

enum RequestDecodePoll<R> {
    Ready(Request<R>),
    Continue,
    Pending(WorkDependency),
    BudgetExhausted,
    Failed(TaskHalt),
}

impl<R: Clone> RequestDecodeWork<R> {
    fn new(request: RuntimeValueRoot) -> Self {
        Self {
            state: RequestDecodeState::Select(request),
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvalContext,
        tags: &Tags,
        specialized: &[SpecializedRequest<R>],
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RequestDecodePoll<R> {
        match &mut self.state {
            RequestDecodeState::Select(request) => {
                let selected = poll_context.evaluate(context, |evaluator| {
                    evaluator.project_root(request, |access, request| {
                        select_request_from_whnf(access, request, tags, specialized)
                    })
                });
                match selected {
                    Ok((selection, payload)) => {
                        self.state = RequestDecodeState::PayloadWhnf {
                            selection,
                            demand: WhnfComputation::from_root(payload),
                        };
                        RequestDecodePoll::Continue
                    }
                    Err(error) => RequestDecodePoll::Failed(error),
                }
            }
            RequestDecodeState::PayloadWhnf { selection, demand } => {
                let payload =
                    match poll_whnf_computation(demand, poll_context, context, step_budget) {
                        WhnfOwnerPoll::Ready(payload) => payload,
                        WhnfOwnerPoll::Pending(dependency) => {
                            return RequestDecodePoll::Pending(dependency);
                        }
                        WhnfOwnerPoll::BudgetExhausted => {
                            return RequestDecodePoll::BudgetExhausted;
                        }
                        WhnfOwnerPoll::Failed(failure) => {
                            return RequestDecodePoll::Failed(TaskHalt::rooted_failure(failure));
                        }
                    };
                let is_list = poll_context.evaluate(context, |evaluator| {
                    evaluator.project_root(&payload, |_, payload| matches!(payload, Value::List(_)))
                });
                if !is_list {
                    return RequestDecodePoll::Failed(TaskHalt::new(
                        "effect request payload must be a list",
                    ));
                }
                let selection = std::mem::replace(selection, RequestSelection::Fail);
                self.state = RequestDecodeState::PayloadItems {
                    selection,
                    front: eval::ListFrontMachine::unowned(payload),
                    arguments: Vec::new(),
                };
                RequestDecodePoll::Continue
            }
            RequestDecodeState::PayloadItems {
                selection: _,
                front,
                arguments,
            } => {
                let front_poll = poll_context.evaluate(context, |evaluator| {
                    front.poll(poll_context, evaluator, context, step_budget)
                });
                match front_poll {
                    eval::ListFrontPoll::Ready(Some((value, tail))) => {
                        arguments.push(value);
                        *front = eval::ListFrontMachine::unowned(tail);
                        RequestDecodePoll::Continue
                    }
                    eval::ListFrontPoll::Ready(None) => self.finish_payload(),
                    eval::ListFrontPoll::Pending(dependency) => {
                        RequestDecodePoll::Pending(dependency)
                    }
                    eval::ListFrontPoll::BudgetExhausted => RequestDecodePoll::BudgetExhausted,
                    eval::ListFrontPoll::Failed(failure) => {
                        RequestDecodePoll::Failed(TaskHalt::rooted_failure(failure))
                    }
                }
            }
            RequestDecodeState::ResumeTask {
                continuation,
                value,
                demand,
            } => {
                let task = match poll_request_id(demand, poll_context, context, step_budget, "task")
                {
                    RequestIdPoll::Ready(id) => id,
                    RequestIdPoll::Pending(dependency) => {
                        return RequestDecodePoll::Pending(dependency);
                    }
                    RequestIdPoll::BudgetExhausted => return RequestDecodePoll::BudgetExhausted,
                    RequestIdPoll::Failed(error) => return RequestDecodePoll::Failed(error),
                };
                let Some(task) = EvaluationTaskId::from_u64(task) else {
                    return RequestDecodePoll::Failed(TaskHalt::new(
                        "reflection task ID must be nonzero",
                    ));
                };
                self.state = RequestDecodeState::ResumeContinuation {
                    task,
                    value: value.clone(),
                    demand: WhnfComputation::from_root(continuation.clone()),
                };
                RequestDecodePoll::Continue
            }
            RequestDecodeState::ResumeContinuation {
                task,
                value,
                demand,
            } => {
                match poll_request_id(demand, poll_context, context, step_budget, "continuation") {
                    RequestIdPoll::Ready(continuation) => RequestDecodePoll::Ready(
                        Request::Resume(*task, continuation, value.clone()),
                    ),
                    RequestIdPoll::Pending(dependency) => RequestDecodePoll::Pending(dependency),
                    RequestIdPoll::BudgetExhausted => RequestDecodePoll::BudgetExhausted,
                    RequestIdPoll::Failed(error) => RequestDecodePoll::Failed(error),
                }
            }
            RequestDecodeState::Handle {
                operation,
                rest,
                demand,
            } => {
                let handle = match poll_whnf_computation(demand, poll_context, context, step_budget)
                {
                    WhnfOwnerPoll::Ready(handle) => handle,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return RequestDecodePoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::BudgetExhausted => return RequestDecodePoll::BudgetExhausted,
                    WhnfOwnerPoll::Failed(failure) => {
                        return RequestDecodePoll::Failed(TaskHalt::rooted_failure(failure));
                    }
                };
                let mut rest = std::mem::take(rest).into_iter();
                let mut next = || {
                    rest.next()
                        .expect("validated request arity must retain its argument")
                };
                match *operation {
                    HandleOperation::Volume(operation) => {
                        let volume = poll_context.evaluate(context, |evaluator| {
                            evaluator.project_root(&handle, |access, handle| {
                                handle_volume(access.values(), &handle)
                            })
                        });
                        if volume.is_none() {
                            return RequestDecodePoll::Failed(TaskHalt::new(
                                "a volume effect needs a handle of this runtime's volume",
                            ));
                        }
                        // The request keeps the handle, and so its volume,
                        // until it completes (`api-volume-cells`).
                        let volume = VolumeTarget::Handle(handle);
                        RequestDecodePoll::Ready(match operation {
                            VolumeOperation::Get => Request::VolumeGet(volume, next()),
                            VolumeOperation::Set => Request::VolumeSet(volume, next(), next()),
                            VolumeOperation::Rewrite => {
                                Request::VolumeRewrite(volume, next(), next())
                            }
                            VolumeOperation::TryGet => Request::VolumeTryGet(volume, next()),
                            VolumeOperation::Del => Request::VolumeDel(volume, next()),
                        })
                    }
                    HandleOperation::Promise(operation) => {
                        // The request holds the breaker until it completes.
                        let promise = poll_context.evaluate(context, |evaluator| {
                            evaluator.project_root(&handle, |access, resolver| {
                                access.values().resolver_root(&resolver)
                            })
                        });
                        let Some(promise) = promise else {
                            return RequestDecodePoll::Failed(TaskHalt::new(
                                "a promise effect needs a resolver of this runtime's promise",
                            ));
                        };
                        RequestDecodePoll::Ready(match operation {
                            PromiseOperation::Status => Request::PromiseStatus(promise),
                            PromiseOperation::Fulfill => Request::PromiseFulfill(promise, next()),
                            PromiseOperation::Break => Request::PromiseBreak(promise, next()),
                        })
                    }
                }
            }
            RequestDecodeState::Poisoned => {
                panic!("request decoder was polled after consuming its terminal state")
            }
        }
    }

    fn finish_payload(&mut self) -> RequestDecodePoll<R> {
        let RequestDecodeState::PayloadItems {
            selection,
            arguments,
            ..
        } = std::mem::replace(&mut self.state, RequestDecodeState::Poisoned)
        else {
            unreachable!("payload completion requires item-collection state")
        };
        if arguments.len() != selection.arity() {
            return RequestDecodePoll::Failed(TaskHalt::new(
                "effect request contained the wrong number of arguments",
            ));
        }
        if let RequestSelection::Handle(operation) = selection {
            let mut arguments = arguments.into_iter();
            let handle = arguments
                .next()
                .expect("handle request arity was checked above");
            self.state = RequestDecodeState::Handle {
                operation,
                rest: arguments.collect(),
                demand: WhnfComputation::from_root(handle),
            };
            return RequestDecodePoll::Continue;
        }
        if matches!(selection, RequestSelection::Resume) {
            let [task, continuation, value]: [RuntimeValueRoot; 3] = arguments
                .try_into()
                .expect("resume request arity was checked above");
            self.state = RequestDecodeState::ResumeTask {
                continuation,
                value,
                demand: WhnfComputation::from_root(task),
            };
            RequestDecodePoll::Continue
        } else {
            RequestDecodePoll::Ready(selection.into_request(arguments))
        }
    }
}

enum RequestIdPoll {
    Ready(u64),
    Pending(WorkDependency),
    BudgetExhausted,
    Failed(TaskHalt),
}

fn poll_request_id(
    demand: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
    kind: &str,
) -> RequestIdPoll {
    let value = match poll_whnf_computation(demand, poll_context, context, step_budget) {
        WhnfOwnerPoll::Ready(value) => value,
        WhnfOwnerPoll::Pending(dependency) => return RequestIdPoll::Pending(dependency),
        WhnfOwnerPoll::BudgetExhausted => return RequestIdPoll::BudgetExhausted,
        WhnfOwnerPoll::Failed(failure) => {
            return RequestIdPoll::Failed(TaskHalt::rooted_failure(failure));
        }
    };
    poll_context.evaluate(context, |evaluator| {
        evaluator.project_root(&value, |_, value| {
            let Value::Number(value) = value else {
                return RequestIdPoll::Failed(TaskHalt::new(format!(
                    "resume request has an invalid {kind} ID"
                )));
            };
            value.to_u64_if_integer().map_or_else(
                || {
                    RequestIdPoll::Failed(TaskHalt::new(format!(
                        "resume request has an invalid {kind} ID"
                    )))
                },
                RequestIdPoll::Ready,
            )
        })
    })
}

fn select_request_from_whnf<R: Clone>(
    access: &EvaluationValueAccess<'_>,
    value: Value,
    tags: &Tags,
    specialized: &[SpecializedRequest<R>],
) -> Result<(RequestSelection<R>, RuntimeValueRoot), TaskHalt> {
    let Value::Dict(dict) = value else {
        return Err(TaskHalt::new("effect API returned a non-request value"));
    };
    let selected = |selection, payload: &Value| {
        Ok((
            selection,
            access.root_value(access.values().duplicate_value(payload)),
        ))
    };
    macro_rules! select {
        ($tag:expr, $selection:expr) => {
            if let Some(payload) = dict.get($tag) {
                return selected($selection, payload);
            }
        };
    }
    select!(&tags.r, RequestSelection::Return);
    select!(&tags.seq, RequestSelection::Seq);
    select!(&tags.alt, RequestSelection::Alt);
    select!(&tags.fail, RequestSelection::Fail);
    select!(&tags.cut, RequestSelection::Cut);
    select!(&tags.trying, RequestSelection::Try);
    select!(&tags.eval, RequestSelection::Eval);
    select!(&tags.fix, RequestSelection::Fix);
    select!(&tags.get, RequestSelection::Get);
    select!(&tags.set, RequestSelection::Set);
    select!(&tags.heap_get, RequestSelection::HeapGet);
    select!(&tags.heap_set, RequestSelection::HeapSet);
    select!(&tags.heap_rewrite, RequestSelection::HeapRewrite);
    select!(&tags.heap_vol, RequestSelection::HeapVolume);
    select!(&tags.try_get, RequestSelection::TryGet);
    select!(&tags.del, RequestSelection::Del);
    select!(&tags.heap_try_get, RequestSelection::HeapTryGet);
    select!(&tags.heap_del, RequestSelection::HeapDel);
    select!(
        &tags.vol_try_get,
        RequestSelection::Handle(HandleOperation::Volume(VolumeOperation::TryGet))
    );
    select!(
        &tags.vol_del,
        RequestSelection::Handle(HandleOperation::Volume(VolumeOperation::Del))
    );
    select!(&tags.vol_new, RequestSelection::VolumeNew);
    select!(
        &tags.vol_get,
        RequestSelection::Handle(HandleOperation::Volume(VolumeOperation::Get))
    );
    select!(
        &tags.vol_set,
        RequestSelection::Handle(HandleOperation::Volume(VolumeOperation::Set))
    );
    select!(
        &tags.vol_rewrite,
        RequestSelection::Handle(HandleOperation::Volume(VolumeOperation::Rewrite))
    );
    select!(&tags.promise_new, RequestSelection::PromiseNew);
    select!(
        &tags.promise_status,
        RequestSelection::Handle(HandleOperation::Promise(PromiseOperation::Status))
    );
    select!(
        &tags.promise_fulfill,
        RequestSelection::Handle(HandleOperation::Promise(PromiseOperation::Fulfill))
    );
    select!(
        &tags.promise_break,
        RequestSelection::Handle(HandleOperation::Promise(PromiseOperation::Break))
    );
    select!(&tags.reset, RequestSelection::Reset);
    select!(&tags.shift, RequestSelection::Shift);
    select!(&tags.exit_success, RequestSelection::ExitSuccess);
    select!(&tags.exit_error, RequestSelection::ExitError);
    select!(&tags.resume, RequestSelection::Resume);
    for specialized in specialized {
        if let Some(payload) = dict.get(&specialized.tag) {
            return selected(
                RequestSelection::Specialized {
                    request: specialized.request.clone(),
                    arity: specialized.arity,
                },
                payload,
            );
        }
    }
    for (tag, payload) in dict.iter() {
        let Some(identity) = parse_volume_request_tag(tag)? else {
            continue;
        };
        return selected(RequestSelection::Volume(identity), payload);
    }
    Err(TaskHalt::new("effect API returned an unknown request"))
}

const VOLUME_REQUEST_PREFIX: [&str; 3] = ["reflection_runtime", "v0", "volume"];

fn volume_request_tag(volume: VolumeId, operation: VolumeOperation) -> Key {
    let operation = match operation {
        VolumeOperation::Get => "get",
        VolumeOperation::Set => "set",
        VolumeOperation::Rewrite => "rewrite",
        VolumeOperation::TryGet => "try_get",
        VolumeOperation::Del => "del",
    };
    Key::abstract_global_path([
        VOLUME_REQUEST_PREFIX[0].to_owned(),
        VOLUME_REQUEST_PREFIX[1].to_owned(),
        VOLUME_REQUEST_PREFIX[2].to_owned(),
        volume.get().to_string(),
        operation.to_owned(),
    ])
}

fn parse_volume_request_tag(tag: &Key) -> Result<Option<VolumeRequestIdentity>, TaskHalt> {
    let Key::AbstractGlobalPath(parts) = tag else {
        return Ok(None);
    };
    if parts.len() < VOLUME_REQUEST_PREFIX.len()
        || !parts
            .iter()
            .zip(VOLUME_REQUEST_PREFIX)
            .all(|(actual, expected)| actual == expected)
    {
        return Ok(None);
    }
    let [_, _, _, volume, operation] = parts.as_ref() else {
        return Err(TaskHalt::new("malformed private volume capability request"));
    };
    let volume = volume
        .parse::<u64>()
        .ok()
        .and_then(VolumeId::from_u64)
        .ok_or_else(|| TaskHalt::new("volume capability has an invalid volume ID"))?;
    let operation = match operation.as_str() {
        "get" => VolumeOperation::Get,
        "set" => VolumeOperation::Set,
        "rewrite" => VolumeOperation::Rewrite,
        "try_get" => VolumeOperation::TryGet,
        "del" => VolumeOperation::Del,
        _ => return Err(TaskHalt::new("volume capability has an invalid operation")),
    };
    Ok(Some(VolumeRequestIdentity { volume, operation }))
}

pub(crate) fn volume_effects(values: &CoreValueFactory, volume: VolumeId) -> PublicValue {
    PublicValue::from_runtime_root(values.construct_runtime_value_root(|access| {
        let entry = |name: &str, operation, arity| {
            (
                Key::atom_from_text(name),
                request_function_in(
                    access,
                    volume_request_tag(volume, operation),
                    arity,
                    Vec::new(),
                    true,
                ),
            )
        };
        Value::Dict(
            [
                entry("get", VolumeOperation::Get, 1),
                entry("set", VolumeOperation::Set, 2),
                entry("rewrite", VolumeOperation::Rewrite, 2),
                entry("try_get", VolumeOperation::TryGet, 1),
                entry("del", VolumeOperation::Del, 1),
            ]
            .into_iter()
            .fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key, value)
            }),
        )
    }))
}

fn effect_api<R: Clone>(
    values: &CoreValueFactory,
    tags: &Tags,
    specs: Vec<EffectRequestSpec<R>>,
    serve_host_families: bool,
    expose_shared_heap: bool,
    expose_exit: bool,
) -> Result<(RuntimeValueRoot, Vec<SpecializedRequest<R>>), TaskHalt> {
    let mut requests = Vec::with_capacity(specs.len());
    let api = values.try_construct_runtime_value_root(|access| {
        let entry = |name: &str, value| (Key::atom_from_text(name), value);
        let heap_api = Value::Dict(
            [
                entry(
                    "get",
                    request_function_in(access, tags.heap_get.clone(), 1, Vec::new(), false),
                ),
                entry(
                    "set",
                    request_function_in(access, tags.heap_set.clone(), 2, Vec::new(), false),
                ),
                entry(
                    "rewrite",
                    request_function_in(access, tags.heap_rewrite.clone(), 2, Vec::new(), false),
                ),
                entry(
                    "try_get",
                    request_function_in(access, tags.heap_try_get.clone(), 1, Vec::new(), false),
                ),
                entry(
                    "del",
                    request_function_in(access, tags.heap_del.clone(), 1, Vec::new(), false),
                ),
                entry("vol", nullary_request(access, tags.heap_vol.clone())),
            ]
            .into_iter()
            .fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key, value)
            }),
        );
        // Volumes as first-class capabilities (`requests-travel-on-ports`).
        let volume_api = Value::Dict(
            [
                entry(
                    "new",
                    request_function_in(access, tags.vol_new.clone(), 1, Vec::new(), false),
                ),
                entry(
                    "get",
                    request_function_in(access, tags.vol_get.clone(), 2, Vec::new(), false),
                ),
                entry(
                    "set",
                    request_function_in(access, tags.vol_set.clone(), 3, Vec::new(), false),
                ),
                entry(
                    "rewrite",
                    request_function_in(access, tags.vol_rewrite.clone(), 3, Vec::new(), false),
                ),
                entry(
                    "try_get",
                    request_function_in(access, tags.vol_try_get.clone(), 2, Vec::new(), false),
                ),
                entry(
                    "del",
                    request_function_in(access, tags.vol_del.clone(), 2, Vec::new(), false),
                ),
            ]
            .into_iter()
            .fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key, value)
            }),
        );
        // Promises, through their resolvers (`requests-travel-on-ports`).
        let promise_api = Value::Dict(
            [
                entry("new", nullary_request(access, tags.promise_new.clone())),
                entry(
                    "status",
                    request_function_in(access, tags.promise_status.clone(), 1, Vec::new(), false),
                ),
                entry(
                    "fulfill",
                    request_function_in(access, tags.promise_fulfill.clone(), 2, Vec::new(), false),
                ),
                entry(
                    "break",
                    request_function_in(access, tags.promise_break.clone(), 2, Vec::new(), false),
                ),
            ]
            .into_iter()
            .fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key, value)
            }),
        );
        // The pure effects, which every effect machine serves.
        let mut entries = vec![
            entry(
                "r",
                request_function_in(access, tags.r.clone(), 1, Vec::new(), false),
            ),
            entry(
                "seq",
                request_function_in(access, tags.seq.clone(), 2, Vec::new(), false),
            ),
            entry(
                "fix",
                request_function_in(access, tags.fix.clone(), 1, Vec::new(), false),
            ),
            entry(
                "get",
                request_function_in(access, tags.get.clone(), 1, Vec::new(), false),
            ),
            entry(
                "set",
                request_function_in(access, tags.set.clone(), 2, Vec::new(), false),
            ),
            entry(
                "try_get",
                request_function_in(access, tags.try_get.clone(), 1, Vec::new(), false),
            ),
            entry(
                "del",
                request_function_in(access, tags.del.clone(), 1, Vec::new(), false),
            ),
            entry(
                "reset",
                request_function_in(access, tags.reset.clone(), 2, Vec::new(), false),
            ),
            entry(
                "shift",
                request_function_in(access, tags.shift.clone(), 2, Vec::new(), false),
            ),
        ];
        entries.extend([
            entry(
                "alt",
                request_function_in(access, tags.alt.clone(), 2, Vec::new(), false),
            ),
            entry("fail", nullary_request(access, tags.fail.clone())),
            entry(
                "cut",
                request_function_in(access, tags.cut.clone(), 1, Vec::new(), false),
            ),
            entry(
                "try",
                request_function_in(access, tags.trying.clone(), 1, Vec::new(), false),
            ),
            entry(
                "eval",
                request_function_in(access, tags.eval.clone(), 1, Vec::new(), false),
            ),
        ]);
        if serve_host_families {
            entries.push(entry("promise", promise_api));
            if expose_shared_heap {
                entries.push(entry("heap", heap_api));
                entries.push(entry("vol", volume_api));
            }
        }
        if expose_exit {
            entries.push(entry(
                "exit",
                Value::Dict(
                    [
                        entry(
                            "success",
                            nullary_request(access, tags.exit_success.clone()),
                        ),
                        entry(
                            "error",
                            request_function_in(
                                access,
                                tags.exit_error.clone(),
                                1,
                                Vec::new(),
                                false,
                            ),
                        ),
                    ]
                    .into_iter()
                    .fold(Dict::new_sync(), |dict, (key, value)| {
                        dict.insert(key, value)
                    }),
                ),
            ));
        }
        let mut api = entries
            .into_iter()
            .fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key, value)
            });
        for spec in specs {
            let tag = Key::abstract_global_path(spec.tag_path.iter().map(Arc::as_ref));
            let api_name = spec
                .api_path
                .as_ref()
                .map(|path| path.iter().map(Arc::as_ref).collect::<Vec<_>>().join("."));
            if requests
                .iter()
                .any(|request: &SpecializedRequest<R>| request.tag == tag)
            {
                return Err(TaskHalt::new(format!(
                    "duplicate private tag for effect API name `{}`",
                    api_name.as_deref().unwrap_or("<hidden>")
                )));
            }
            if let Some(path) = &spec.api_path {
                let value = if spec.arity == 0 {
                    nullary_request(access, tag.clone())
                } else {
                    request_function_in(access, tag.clone(), spec.arity, Vec::new(), false)
                };
                api = insert_effect_api_path(
                    access,
                    api,
                    path,
                    value,
                    api_name
                        .as_deref()
                        .expect("visible request must have a name"),
                )?;
            }
            requests.push(SpecializedRequest {
                tag,
                arity: spec.arity,
                request: spec.request,
            });
        }
        Ok::<_, TaskHalt>(Value::Dict(api))
    })?;
    Ok((api, requests))
}

fn insert_effect_api_path(
    access: &RuntimeValueAccess<'_>,
    api: Dict,
    path: &[Arc<str>],
    value: Value,
    display_path: &str,
) -> Result<Dict, TaskHalt> {
    if path.is_empty() {
        return Err(TaskHalt::new("effect API path must not be empty"));
    }

    let mut parents = Vec::with_capacity(path.len().saturating_sub(1));
    let mut current = api;
    for (index, name) in path.iter().enumerate() {
        let key = Key::atom_from_text(name);
        if index + 1 == path.len() {
            if current.get(&key).is_some() {
                return Err(TaskHalt::new(format!(
                    "duplicate effect API name `{display_path}`"
                )));
            }
            current = current.insert(key, value);
            break;
        }

        let nested = match current.get(&key).map(|value| access.duplicate_value(value)) {
            Some(Value::Dict(nested)) => nested,
            Some(_) => {
                return Err(TaskHalt::new(format!(
                    "effect API path `{display_path}` crosses non-dictionary `{name}`"
                )));
            }
            None => Dict::new_sync(),
        };
        parents.push((current, key));
        current = nested;
    }

    while let Some((parent, key)) = parents.pop() {
        current = parent.insert(key, Value::Dict(current));
    }
    Ok(current)
}

fn request_function_in(
    access: &RuntimeValueAccess<'_>,
    tag: Key,
    arity: usize,
    supplied: Vec<Value>,
    wrap_effect: bool,
) -> Value {
    let remaining = arity - supplied.len();
    let mut net = NetBuilder::<CoreSpecialization>::new();
    let exposed = net.unary_operator(eval::request_operator(
        access,
        tag,
        arity,
        Arc::from(supplied),
        wrap_effect,
    ));
    let template = net.finish(exposed);
    Value::Function(FunctionValue::new(
        NetValue::new(
            access
                .construct_managed_core_net(template.instantiate_with(access))
                .expect("managed core-net representation must fit one collector run"),
        ),
        remaining,
    ))
}

fn nullary_request(_access: &RuntimeValueAccess<'_>, tag: Key) -> Value {
    Value::Dict(Dict::new_sync().insert(tag, Value::List(List::empty())))
}

fn alternative_returns_root(
    access: &RuntimeValueAccess<'_>,
    tags: &Tags,
    values: &[PublicValue],
) -> RuntimeValueRoot {
    access.root_runtime_value(
        values
            .iter()
            .rev()
            .map(|value| {
                eval::constant_effect_in(
                    access,
                    request_value(
                        access,
                        &tags.r,
                        vec![value.clone().into_runtime_root().clone_core_with(access)],
                    ),
                )
            })
            .reduce(|right, left| {
                eval::constant_effect_in(
                    access,
                    request_value(access, &tags.alt, vec![left, right]),
                )
            })
            .expect("alternative return construction requires at least two values"),
    )
}

fn missing_volume_error(volume: VolumeId) -> TaskHalt {
    TaskHalt::new(format!(
        "reflection volume {} was revoked before its edits committed",
        volume.get()
    ))
}

fn lazy_value_path_root(
    access: &RuntimeValueAccess<'_>,
    value: &RuntimeValueRoot,
    path: &[Key],
) -> RuntimeValueRoot {
    let value = value.clone_core_with(access);
    if path.is_empty() {
        return access.root_runtime_value(value);
    }
    access.root_runtime_value(Value::Lazy(LazyValue::from_access_in(
        access,
        Arc::from(
            path.iter()
                .cloned()
                .map(CoreDataKey::Key)
                .collect::<Vec<_>>(),
        ),
        Arc::from([value]),
    )))
}

fn reset_stack_root_in(
    context: &EvaluatorStepContext<'_>,
    state: &RuntimeValueRoot,
    continuation_state: &Key,
) -> Result<RuntimeValueRoot, TaskHalt> {
    context.project_root(state, |access, state| {
        let Value::Dict(state) = state else {
            return Err(TaskHalt::new("reflection user state must be a dictionary"));
        };
        Ok(access.root_value(
            state
                .get(continuation_state)
                .map(|value| access.values().duplicate_value(value))
                .unwrap_or_else(|| Value::List(List::empty())),
        ))
    })
}

fn encode_reset_frames(access: &EvaluationValueAccess<'_>, frames: &[ResetFrame]) -> Value {
    Value::List(List::from_values(
        frames
            .iter()
            .map(|frame| {
                Value::List(List::from_values(vec![
                    frame.key.to_value_in(access.values()),
                    access.clone_root(&frame.continuation),
                    Value::Number(Number::from_usize(frame.scope_depth)),
                    Value::Number(Number::from_usize(frame.order)),
                ]))
            })
            .collect(),
    ))
}

fn encode_reset_frames_in_state(
    access: &EvaluationValueAccess<'_>,
    state: Value,
    continuation_state: &Key,
    frames: &[ResetFrame],
) -> RuntimeValueRoot {
    let Value::Dict(state) = state else {
        return access.root_value(Value::Lazy(LazyValue::error_in(
            access.values(),
            "reflection user state must remain a dictionary",
        )));
    };
    access.root_value(Value::Dict(state.insert(
        continuation_state.clone(),
        encode_reset_frames(access, frames),
    )))
}

#[cfg(test)]
mod tests;
