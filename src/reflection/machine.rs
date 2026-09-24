use std::collections::HashMap;
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
use super::store::{StoreJournal, VolumeId};
use crate::api::{EvaluatedValue, Value as PublicValue, Values};
use crate::core::{
    Atom, Builtin, CoreValueFactory, Dict, EvaluationFailure, EvaluationHalt, FunctionValue, Key,
    LazyValue, List, NetValue, PromisedValue, RuntimeValueAccess, Value, keys,
};
use crate::core_net::{CoreDataKey, CoreSpecialization};
use crate::eval;
use crate::eval::whnf::WhnfComputation;
#[cfg(test)]
use crate::evaluation::OwnedEvalContext;
use crate::evaluation::{
    EvalContext, EvaluationExitBlock, EvaluationMachinePoll, EvaluationPollContext,
    EvaluationPumpOutcome, EvaluationSession, EvaluationTaskBlock, EvaluationTaskId,
    EvaluationTaskMachine, EvaluationWaitPoll, EvaluatorStepContext, ExactDemandRoute, ExitIntent,
    WhnfOwnerPoll, WorkDependency, poll_whnf_computation,
};
use crate::interaction_net::NetBuilder;
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

#[cfg(test)]
use super::protocol::HostSnapshot;

mod reset_stack;

use reset_stack::{ResetStackMachine, ResetStackPoll};

#[derive(Clone)]
struct Tags {
    r: Key,
    seq: Key,
    alt: Key,
    fail: Key,
    cut: Key,
    fix: Key,
    get: Key,
    set: Key,
    heap_get: Key,
    heap_set: Key,
    heap_rewrite: Key,
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
            fix: tag("fix"),
            get: tag("get"),
            set: tag("set"),
            heap_get: tag("heap_get"),
            heap_set: tag("heap_set"),
            heap_rewrite: tag("heap_rewrite"),
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
    continuations: HashMap<u64, CapturedContinuation>,
    search: SearchPolicy<Branch<S>, IsolatedSearchBranch<S>>,
    execution: TaskExecution<S>,
    blocked: Option<BlockedExecution<S>>,
    exit: Option<TaskExitState<S>>,
    terminal: Option<TaskTerminal>,
    exact_demand_route: ExactDemandRoute,
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
        let mut task = Self::new_in_context(effect, specialization, host, eval_context)?;
        task._demand_owner = Some(owner);
        Ok(task)
    }

    pub(super) fn new_in_context(
        effect: Value,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
    ) -> Result<Self, TaskHalt> {
        Self::new_in_context_with_policy(effect, specialization, host, eval_context, false)
    }

    pub(super) fn new_isolated_in_context(
        effect: Value,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
    ) -> Result<Self, TaskHalt> {
        Self::new_in_context_with_policy(effect, specialization, host, eval_context, true)
    }

    fn new_in_context_with_policy(
        effect: Value,
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
        effect: Value,
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
            specialization.exposes_shared_heap(),
            exposes_exit,
        )?;
        let id = eval_context
            .task_id()
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
        let initial_state = Value::Dict(Dict::new_sync());
        let root = Branch::new(eval_context.values(), effect, initial_state);
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
            continuations: HashMap::new(),
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
        let diagnostic_context =
            branch.root_value(&values, Value::binary_from_text(&diagnostic_context));
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
        continuation: CapturedContinuation,
    ) -> Result<RuntimeValueRoot, TaskHalt> {
        let id = self.next_continuation;
        self.next_continuation = self
            .next_continuation
            .checked_add(1)
            .ok_or_else(|| TaskHalt::new("reflection continuation IDs exhausted"))?;
        self.continuations.insert(id, continuation);
        Ok(self
            .eval_context
            .values()
            .construct_runtime_value_root(|access| {
                request_function_in(
                    access,
                    self.tags.resume.clone(),
                    3,
                    vec![
                        Value::Number(Number::from_u64(self.id.get())),
                        Value::Number(Number::from_u64(id)),
                    ],
                    true,
                )
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

    pub(super) fn run(&mut self) -> Result<TaskOutcome, TaskHalt> {
        loop {
            match self.poll(256) {
                EffectTaskPoll::Yielded => {}
                EffectTaskPoll::Blocked(blocked) => {
                    if let Some(dependency) = blocked.dependency {
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
                        match self.eval_context.pump_wait_on_route(
                            &wait,
                            4_096,
                            &mut self.exact_demand_route,
                        ) {
                            EvaluationPumpOutcome::TargetReady
                            | EvaluationPumpOutcome::BudgetExhausted => continue,
                            EvaluationPumpOutcome::Busy => {
                                self.eval_context.wait_for_claimed_task_on_route(
                                    &wait,
                                    &mut self.exact_demand_route,
                                );
                                continue;
                            }
                            EvaluationPumpOutcome::NoProgress
                                if blocked.observed_generation.is_none() =>
                            {
                                if self
                                    .eval_context
                                    .wait_for_observed_dependency_progress_on_route(
                                        &wait,
                                        &mut self.exact_demand_route,
                                    )
                                {
                                    continue;
                                }
                                let error = TaskHalt::new(
                                    "synchronous reflection task has no runnable producer for its dependency",
                                );
                                self.finish(TaskTerminal::Failed(error.clone()));
                                return Err(error);
                            }
                            EvaluationPumpOutcome::NoProgress => {}
                        }
                    }
                    let generation = blocked.observed_generation.ok_or_else(|| {
                        TaskHalt::new("blocked reflection task has no wake condition")
                    })?;
                    if !self.host.wait_for_change(generation) {
                        self.finish(TaskTerminal::Cancelled);
                    }
                }
                EffectTaskPoll::Complete(value) => return Ok(TaskOutcome::Complete(value)),
                EffectTaskPoll::Failed(error) => return Err(error),
                EffectTaskPoll::Cancelled => return Ok(TaskOutcome::Cancelled),
                EffectTaskPoll::Exit(_) => {
                    unreachable!("direct effect-run profiles do not expose runtime exit")
                }
            }
        }
    }

    pub(super) fn poll(&mut self, steps: usize) -> EffectTaskPoll {
        let context = EvaluationPollContext::for_context(&self.eval_context);
        for _ in 0..steps.max(1) {
            let mut budget = crate::evaluation::EvaluationStepBudget::new(steps.max(1));
            let poll = self.poll_with_context(&context, &mut budget);
            let EffectTaskPoll::Blocked(blocked) = &poll else {
                return poll;
            };
            if let Some(error) = blocked
                .dependency
                .as_ref()
                .and_then(|dependency| self.eval_context.recursive_promise_dependency(dependency))
            {
                let error = TaskHalt::new(error);
                self.finish(TaskTerminal::Failed(error.clone()));
                return EffectTaskPoll::Failed(error);
            }
            let Some(WorkDependency::Wait(wait)) = &blocked.dependency else {
                return poll;
            };
            match self.eval_context.pump_wait_on_route(
                wait,
                steps.max(1),
                &mut self.exact_demand_route,
            ) {
                EvaluationPumpOutcome::TargetReady => {}
                EvaluationPumpOutcome::BudgetExhausted => return EffectTaskPoll::Yielded,
                EvaluationPumpOutcome::Busy | EvaluationPumpOutcome::NoProgress => {
                    if let Some(error) = blocked.dependency.as_ref().and_then(|dependency| {
                        self.eval_context.recursive_promise_dependency(dependency)
                    }) {
                        let error = TaskHalt::new(error);
                        self.finish(TaskTerminal::Failed(error.clone()));
                        return EffectTaskPoll::Failed(error);
                    }
                    return poll;
                }
            }
        }
        EffectTaskPoll::Yielded
    }

    pub(super) fn poll_with_context(
        &mut self,
        context: &EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EffectTaskPoll {
        context.assert_context(&self.eval_context);
        if let Some(terminal) = &self.terminal {
            return terminal.poll();
        }
        if let Some(poll) = self.poll_exit() {
            return poll;
        }
        if let Some(blocked) = self.poll_blocked() {
            return blocked;
        }

        while step_budget.remaining() != 0 {
            if let Some(controlling) = self.execution.controlling.take() {
                let previous_remaining = step_budget.remaining();
                match self.control_step(context, controlling, step_budget) {
                    ControlStep::Continue(controlling) => {
                        self.execution.controlling = Some(controlling);
                    }
                    ControlStep::Complete(work) => self.execution.work = work,
                    ControlStep::Blocked(controlling, dependency) => {
                        self.execution.controlling = Some(controlling);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked(blocked) {
                            return poll;
                        }
                    }
                    ControlStep::Yielded(controlling) => {
                        self.execution.controlling = Some(controlling);
                        return EffectTaskPoll::Yielded;
                    }
                    ControlStep::Failed(controlling, error) => {
                        self.execution.controlling = Some(controlling);
                        return self.handle_step_error(error);
                    }
                }
                step_budget.charge_if_unchanged(previous_remaining);
                continue;
            }
            if let Some(specializing) = self.execution.specializing.take() {
                let previous_remaining = step_budget.remaining();
                match self.specialization_step(context, specializing, step_budget) {
                    SpecializationStep::Continue(specializing) => {
                        self.execution.specializing = Some(specializing);
                    }
                    SpecializationStep::Complete(work) => self.execution.work = *work,
                    SpecializationStep::Blocked(specializing, dependency) => {
                        self.execution.specializing = Some(specializing);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked(blocked) {
                            return poll;
                        }
                    }
                    SpecializationStep::Yielded(specializing) => {
                        self.execution.specializing = Some(specializing);
                        return EffectTaskPoll::Yielded;
                    }
                    SpecializationStep::Failed(specializing, error) => {
                        self.execution.specializing = Some(specializing);
                        return self.handle_step_error(error);
                    }
                }
                step_budget.charge_if_unchanged(previous_remaining);
                continue;
            }
            if let Some(demanding) = self.execution.demanding.take() {
                let previous_remaining = step_budget.remaining();
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
                            if let Some(poll) = self.install_blocked(blocked) {
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
                            self.finish(terminal);
                            return self.terminal.as_ref().expect("terminal set above").poll();
                        }
                    },
                    ScalarDemandStep::Blocked(demanding, dependency) => {
                        self.execution.demanding = Some(demanding);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked(blocked) {
                            return poll;
                        }
                    }
                    ScalarDemandStep::Yielded(demanding) => {
                        self.execution.demanding = Some(demanding);
                        return EffectTaskPoll::Yielded;
                    }
                    ScalarDemandStep::Failed(demanding, error) => {
                        self.execution.demanding = Some(demanding);
                        return self.handle_step_error(error);
                    }
                }
                step_budget.charge_if_unchanged(previous_remaining);
                continue;
            }
            if let Some(pathing) = self.execution.pathing.take() {
                let previous_remaining = step_budget.remaining();
                match self.state_path_step(context, pathing, step_budget) {
                    StatePathStep::Continue(pathing) => self.execution.pathing = Some(pathing),
                    StatePathStep::Complete(work) => self.execution.work = work,
                    StatePathStep::Blocked(pathing, dependency) => {
                        self.execution.pathing = Some(pathing);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked(blocked) {
                            return poll;
                        }
                    }
                    StatePathStep::Yielded(pathing) => {
                        self.execution.pathing = Some(pathing);
                        return EffectTaskPoll::Yielded;
                    }
                    StatePathStep::Failed(pathing, error) => {
                        self.execution.pathing = Some(pathing);
                        return self.handle_step_error(error);
                    }
                }
                step_budget.charge_if_unchanged(previous_remaining);
                continue;
            }
            if let Some(decoding) = self.execution.decoding.take() {
                let previous_remaining = step_budget.remaining();
                match self.decode_step(context, decoding, step_budget) {
                    EffectDecodeStep::Continue(decoding) => {
                        self.execution.decoding = Some(decoding);
                    }
                    EffectDecodeStep::Complete(work) => self.execution.work = work,
                    EffectDecodeStep::Blocked(decoding, dependency) => {
                        self.execution.decoding = Some(decoding);
                        let blocked = BlockedExecution::waiting_on(dependency, self.retry_wake());
                        if let Some(poll) = self.install_blocked(blocked) {
                            return poll;
                        }
                    }
                    EffectDecodeStep::Yielded(decoding) => {
                        self.execution.decoding = Some(decoding);
                        return EffectTaskPoll::Yielded;
                    }
                    EffectDecodeStep::Failed(decoding, error) => {
                        self.execution.decoding = Some(decoding);
                        return self.handle_step_error(error);
                    }
                }
                step_budget.charge_if_unchanged(previous_remaining);
                continue;
            }
            assert!(step_budget.try_consume());
            let work = self.execution.work.clone();
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
                    if let Some(poll) = self.install_blocked(blocked) {
                        return poll;
                    }
                }
                Ok(MachineStep::Terminal(terminal)) => {
                    self.finish(terminal);
                    return self.terminal.as_ref().expect("terminal set above").poll();
                }
                Ok(MachineStep::Exit(intent)) => {
                    let exit = self.prepare_exit(intent);
                    let poll = exit.poll.clone();
                    self.exit = Some(exit);
                    return EffectTaskPoll::Exit(poll);
                }
                Err(error) => return self.handle_step_error(error),
            }
        }
        EffectTaskPoll::Yielded
    }

    fn handle_step_error(&mut self, error: TaskHalt) -> EffectTaskPoll {
        if let Some(wait) = error.blocked_on() {
            let blocked = self.waiting_block(WorkDependency::Wait(wait.clone()));
            return self
                .install_blocked(blocked)
                .unwrap_or(EffectTaskPoll::Yielded);
        }
        if let Some(retry) = self.retry_wake() {
            let blocked =
                BlockedExecution::evaluation_error(error, retry, self.eval_context.values());
            return self
                .install_blocked(blocked)
                .unwrap_or(EffectTaskPoll::Yielded);
        }
        self.finish(TaskTerminal::Failed(error));
        self.terminal.as_ref().expect("terminal set above").poll()
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
                RequestDecodePoll::Yielded => EffectDecodeStep::Yielded(decoding),
                RequestDecodePoll::Failed(error) => {
                    let error = decoding.contextualize(error);
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
            WhnfOwnerPoll::Yielded => EffectDecodeStep::Yielded(decoding),
            WhnfOwnerPoll::External(boundary) => {
                let error = decoding.contextualize(TaskHalt::new(format!(
                    "reflection effect decoding reached an unsupported {boundary:?} boundary"
                )));
                EffectDecodeStep::Failed(decoding, error)
            }
            WhnfOwnerPoll::Failed(failure) => {
                let error = decoding.contextualize(TaskHalt::rooted_failure(failure));
                EffectDecodeStep::Failed(decoding, error)
            }
            WhnfOwnerPoll::Ready(value) => {
                self.complete_decode_phase(context, decoding, purpose, value)
            }
        }
    }

    fn specialization_step(
        &mut self,
        context: &EvaluationPollContext,
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
                    let values = Values::from_core_factory(self.eval_context.values().clone());
                    SpecializationRequestInput::Value(EvaluatedValue::from_whnf(
                        &values,
                        PublicValue::from_runtime_root(value),
                    ))
                }
                WhnfOwnerPoll::Pending(dependency) => {
                    return SpecializationStep::Blocked(specializing, dependency);
                }
                WhnfOwnerPoll::Yielded => {
                    return SpecializationStep::Yielded(specializing);
                }
                WhnfOwnerPoll::Failed(failure) => {
                    SpecializationRequestInput::Failed(TaskHalt::rooted_failure(failure))
                }
                WhnfOwnerPoll::External(boundary) => {
                    SpecializationRequestInput::Failed(TaskHalt::new(format!(
                        "specialized reflection request reached an unsupported {boundary:?} boundary"
                    )))
                }
            };
            specializing.demand = None;
            specializing.input = Some(input);
        }

        let checkpoint = specializing.branch.retry_candidate();
        let mut activity = RequestActivity::default();
        let result = specializing.request.poll(
            &self.specialization,
            specializing.input.take(),
            &mut RequestContext {
                eval_context: &self.eval_context,
                host: &self.host,
                transaction: specializing.branch.transaction.as_mut(),
                activity: &mut activity,
            },
        );
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
                SpecializationStep::Complete(Box::new(self.complete_specialization_request(
                    result,
                    specializing.branch,
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
            WhnfOwnerPoll::Yielded => return ScalarDemandStep::Yielded(demanding),
            WhnfOwnerPoll::Failed(failure) => {
                return ScalarDemandStep::Failed(demanding, TaskHalt::rooted_failure(failure));
            }
            WhnfOwnerPoll::External(boundary) => {
                return ScalarDemandStep::Failed(
                    demanding,
                    TaskHalt::new(format!(
                        "reflection scalar demand reached an unsupported {boundary:?} boundary"
                    )),
                );
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
                    let value = evaluator.project_root(&value);
                    if value != self.eval_context.values().unit() {
                        return Err(TaskHalt::new(format!(
                            "effect task returned {}; expected unit",
                            value.diagnostic_kind_name()
                        )));
                    }
                    Ok(())
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
                    let value = evaluator.project_root(&value);
                    if value != self.eval_context.values().unit() {
                        return Err(TaskHalt::new(format!(
                            "scoped effect close must return unit, got {value:?}"
                        )));
                    }
                    Ok(())
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
                    eval::ConversionPoll::Yielded => {
                        controlling.operation = ControlOperation::Key {
                            key,
                            stack,
                            disposition,
                        };
                        ControlStep::Yielded(controlling)
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
                            let continuation =
                                match self.capture_continuation(CapturedContinuation {
                                    sequence: std::mem::take(&mut branch.control.sequence),
                                    delimiters: Vec::new(),
                                    reset_frames: Vec::new(),
                                }) {
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
                                let state = evaluator.project_root(&branch.state);
                                encode_reset_frames_in_state(
                                    evaluator,
                                    state,
                                    &self.tags.continuation_state,
                                    &frames,
                                )
                            });
                            branch.set_state(self.eval_context.values(), state);
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
                            let continuation =
                                match self.capture_continuation(CapturedContinuation {
                                    sequence: std::mem::take(&mut branch.control.sequence),
                                    delimiters: inner_delimiters,
                                    reset_frames: inner_reset_frames,
                                }) {
                                    Ok(continuation) => continuation,
                                    Err(error) => {
                                        return ControlStep::Failed(
                                            ControlWork::poisoned(branch, scope_depth),
                                            error,
                                        );
                                    }
                                };
                            let state = context.evaluate(&self.eval_context, |evaluator| {
                                let state = evaluator.project_root(&branch.state);
                                encode_reset_frames_in_state(
                                    evaluator,
                                    state,
                                    &self.tags.continuation_state,
                                    &frames,
                                )
                            });
                            branch.set_state(self.eval_context.values(), state);
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
                ResetStackPoll::Yielded => {
                    controlling.operation = ControlOperation::Stack {
                        key,
                        stack,
                        disposition,
                    };
                    ControlStep::Yielded(controlling)
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
                        let state = evaluator.project_root(&branch.state);
                        encode_reset_frames_in_state(
                            evaluator,
                            state,
                            &self.tags.continuation_state,
                            &frames,
                        )
                    });
                    self.next_control_order = next_order;
                    branch.set_state(self.eval_context.values(), state);
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
                ResetStackPoll::Yielded => {
                    controlling.operation = ControlOperation::InstallCaptured {
                        stack,
                        captured,
                        value,
                    };
                    ControlStep::Yielded(controlling)
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
                        let state = evaluator.project_root(&branch.state);
                        encode_reset_frames_in_state(
                            evaluator,
                            state,
                            &self.tags.continuation_state,
                            &[],
                        )
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
                    let handle = match PromisedValue::fixpoint(
                        &self.eval_context,
                        "reflection effect fixpoint",
                    ) {
                        Ok(handle) => handle,
                        Err(error) => {
                            return ControlStep::Failed(
                                ControlWork::poisoned(branch, root.scope_depth),
                                TaskHalt::new(error.as_ref()),
                            );
                        }
                    };
                    let marker = branch
                        .root_value(self.eval_context.values(), Value::Promised(handle.clone()));
                    let outer_control = std::mem::take(&mut branch.control);
                    branch.set_state(self.eval_context.values(), state);
                    let handle = self
                        .eval_context
                        .values()
                        .with_runtime_value_access(|access| handle.root_in(&access));
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
                ResetStackPoll::Yielded => {
                    controlling.operation = ControlOperation::StartFixpoint {
                        stack,
                        root,
                        choices,
                    };
                    ControlStep::Yielded(controlling)
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
                                let state = evaluator.project_root(&branch.state);
                                encode_reset_frames_in_state(
                                    evaluator,
                                    state,
                                    &self.tags.continuation_state,
                                    &resets,
                                )
                            });
                            branch.set_state(self.eval_context.values(), state);
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
                    ResetStackPoll::Yielded => {
                        controlling.operation = ControlOperation::Delivery { stack, value };
                        ControlStep::Yielded(controlling)
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
                        let Value::Dict(state) = evaluator.project_root(&branch.state) else {
                            return Err(TaskHalt::new(
                                "reflection user state must be a dictionary",
                            ));
                        };
                        Ok(evaluator.root_value(Value::Dict(state.insert(
                            self.tags.continuation_state.clone(),
                            evaluator.project_root(&decoded.serialized),
                        ))))
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
                ResetStackPoll::Yielded => {
                    controlling.operation = ControlOperation::Restore {
                        stack,
                        outer,
                        value,
                    };
                    ControlStep::Yielded(controlling)
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
        context: &EvaluationPollContext,
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
                            return self.store_path_step(
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
                    eval::ConversionPoll::Yielded => StatePathStep::Yielded(pathing),
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
                    ValuePathPoll::Yielded => StatePathStep::Yielded(pathing),
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
                        matches!(evaluator.project_root(&base), Value::Dict(_))
                    });
                    if !is_dict {
                        return StatePathStep::Failed(
                            pathing,
                            TaskHalt::new("reflection user state must be a dictionary"),
                        );
                    }
                    if path.is_empty() {
                        pathing.branch.state = base;
                        return StatePathStep::Complete(MachineWork::deliver(
                            self.eval_context.values(),
                            self.eval_context.values().unit(),
                            pathing.branch,
                            pathing.scope_depth,
                        ));
                    }
                    let update = context.evaluate(&self.eval_context, |evaluator| {
                        let path = Value::List(List::from_values(
                            path.iter()
                                .map(|key| key.to_value_with(self.eval_context.values()))
                                .collect(),
                        ));
                        let value = evaluator.project_root(value);
                        let base = evaluator.project_root(&base);
                        let update = evaluator.construct_lazy_value(|access| {
                            Value::builtin_call_in(
                                access,
                                Builtin::DictUpdate,
                                vec![path, value, base],
                            )
                        });
                        evaluator.root_value(update)
                    });
                    pathing.operation =
                        StatePathOperation::SetUpdate(WhnfComputation::from_root(update));
                    StatePathStep::Continue(pathing)
                }
                WhnfOwnerPoll::Pending(dependency) => StatePathStep::Blocked(pathing, dependency),
                WhnfOwnerPoll::Yielded => StatePathStep::Yielded(pathing),
                WhnfOwnerPoll::Failed(failure) => {
                    StatePathStep::Failed(pathing, TaskHalt::rooted_failure(failure))
                }
                WhnfOwnerPoll::External(boundary) => StatePathStep::Failed(
                    pathing,
                    TaskHalt::new(format!(
                        "reflection state path reached an unsupported {boundary:?} boundary"
                    )),
                ),
            },
            StatePathOperation::SetUpdate(computation) => {
                match poll_whnf_computation(computation, context, &self.eval_context, step_budget) {
                    WhnfOwnerPoll::Ready(state) => {
                        pathing.branch.state = state;
                        StatePathStep::Complete(MachineWork::deliver(
                            self.eval_context.values(),
                            self.eval_context.values().unit(),
                            pathing.branch,
                            pathing.scope_depth,
                        ))
                    }
                    WhnfOwnerPoll::Pending(dependency) => {
                        StatePathStep::Blocked(pathing, dependency)
                    }
                    WhnfOwnerPoll::Yielded => StatePathStep::Yielded(pathing),
                    WhnfOwnerPoll::Failed(failure) => {
                        StatePathStep::Failed(pathing, TaskHalt::rooted_failure(failure))
                    }
                    WhnfOwnerPoll::External(boundary) => StatePathStep::Failed(
                        pathing,
                        TaskHalt::new(format!(
                            "reflection state update reached an unsupported {boundary:?} boundary"
                        )),
                    ),
                }
            }
            StatePathOperation::Poisoned => {
                unreachable!("failed state-path work cannot be resumed before error handling")
            }
        }
    }

    fn store_path_step(
        &mut self,
        operation: StorePathOperation,
        path: Vec<Key>,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> StatePathStep<S> {
        let result = match operation {
            StorePathOperation::HeapGet => {
                let checkpoint = branch.retry_candidate();
                let values =
                    crate::api::Values::from_core_factory(self.eval_context.values().clone());
                let heap = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let observed = transaction.store.observe_read(&path);
                    let heap = values.clone_core(&transaction.store.view());
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    heap
                } else {
                    let snapshot = self.host.snapshot();
                    values.clone_core(snapshot.store().root())
                };
                let heap = match heap {
                    Ok(heap) => heap,
                    Err(error) => {
                        return StatePathStep::Failed(
                            StatePathWork::poisoned(branch, scope_depth),
                            TaskHalt::from(error),
                        );
                    }
                };
                let value = lazy_value_path_root(&self.eval_context, heap, &path);
                return StatePathStep::Complete(MachineWork::deliver_root(
                    value,
                    branch,
                    scope_depth,
                ));
            }
            StorePathOperation::HeapSet(value) => {
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction
                        .store
                        .write(path, PublicValue::from_runtime_root(value));
                    return StatePathStep::Complete(MachineWork::deliver(
                        self.eval_context.values(),
                        self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    ));
                }
                let snapshot = self.host.snapshot();
                let mut store = StoreJournal::new(snapshot.store().clone());
                store.write(path, PublicValue::from_runtime_root(value));
                self.host.commit(TaskCommit::new(
                    store,
                    snapshot.extra().clone(),
                    S::Journal::default(),
                ))
            }
            StorePathOperation::HeapRewrite(updater) => {
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction
                        .store
                        .rewrite(path, PublicValue::from_runtime_root(updater));
                    return StatePathStep::Complete(MachineWork::deliver(
                        self.eval_context.values(),
                        self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    ));
                }
                let snapshot = self.host.snapshot();
                let mut store = StoreJournal::new(snapshot.store().clone());
                store.rewrite(path, PublicValue::from_runtime_root(updater));
                self.host.commit(TaskCommit::new(
                    store,
                    snapshot.extra().clone(),
                    S::Journal::default(),
                ))
            }
            StorePathOperation::VolumeGet(volume) => {
                let checkpoint = branch.retry_candidate();
                let root = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let observed = transaction.store.observe_volume_read(volume, &path);
                    let root = transaction.store.volume_view(volume);
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    root
                } else {
                    let snapshot = self.host.snapshot();
                    snapshot.store().volume(volume).cloned()
                };
                let value = match root {
                    Some(root) => {
                        let values = crate::api::Values::from_core_factory(
                            self.eval_context.values().clone(),
                        );
                        let root = match values.clone_core(&root) {
                            Ok(root) => root,
                            Err(error) => {
                                return StatePathStep::Failed(
                                    StatePathWork::poisoned(branch, scope_depth),
                                    TaskHalt::from(error),
                                );
                            }
                        };
                        lazy_value_path_root(&self.eval_context, root, &path)
                    }
                    None => self
                        .eval_context
                        .values()
                        .construct_runtime_value_root(|access| {
                            Value::Lazy(LazyValue::error_in(
                                access,
                                format!("reflection volume {} has been revoked", volume.get()),
                            ))
                        }),
                };
                return StatePathStep::Complete(MachineWork::deliver_root(
                    value,
                    branch,
                    scope_depth,
                ));
            }
            StorePathOperation::VolumeSet(volume, value) => {
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction.store.write_volume(
                        volume,
                        path,
                        PublicValue::from_runtime_root(value),
                    );
                    return StatePathStep::Complete(MachineWork::deliver(
                        self.eval_context.values(),
                        self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    ));
                }
                let snapshot = self.host.snapshot();
                let mut store = StoreJournal::new(snapshot.store().clone());
                store.write_volume(volume, path, PublicValue::from_runtime_root(value));
                self.host.commit(TaskCommit::new(
                    store,
                    snapshot.extra().clone(),
                    S::Journal::default(),
                ))
            }
            StorePathOperation::VolumeRewrite(volume, updater) => {
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction.store.rewrite_volume(
                        volume,
                        path,
                        PublicValue::from_runtime_root(updater),
                    );
                    return StatePathStep::Complete(MachineWork::deliver(
                        self.eval_context.values(),
                        self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    ));
                }
                let snapshot = self.host.snapshot();
                let mut store = StoreJournal::new(snapshot.store().clone());
                store.rewrite_volume(volume, path, PublicValue::from_runtime_root(updater));
                self.host.commit(TaskCommit::new(
                    store,
                    snapshot.extra().clone(),
                    S::Journal::default(),
                ))
            }
        };
        StatePathStep::Complete(match result {
            CommitResult::Committed => {
                branch.retry = None;
                MachineWork::deliver(
                    self.eval_context.values(),
                    self.eval_context.values().unit(),
                    branch,
                    scope_depth,
                )
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
                    let effect = evaluator.project_root(&value);
                    let Value::Dict(effect) = effect else {
                        return Err(TaskHalt::new(format!(
                            "reflection task requires an effect object, got {effect:?}"
                        )));
                    };
                    effect
                        .get(&*keys::EFF)
                        .cloned()
                        .map(|function| evaluator.root_value(function))
                        .ok_or_else(|| TaskHalt::new("reflection effect has no `eff` member"))
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
        context: &EvaluationPollContext,
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
        context: &EvaluationPollContext,
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
        context: &EvaluationPollContext,
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

    fn interpret_request(
        &mut self,
        context: &EvaluationPollContext,
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
            Request::Alt(left, right) => {
                if (scope_depth > 0 || self.search.retains_all()) && !branch.active_fixes.is_empty()
                {
                    let inherited_restarts = branch.fix_restarts.clone();
                    let active = branch
                        .active_fixes
                        .first_mut()
                        .expect("checked nonempty fixpoint stack");
                    if let Some(choice) = active.choices.get(active.next_choice).copied() {
                        active.next_choice += 1;
                        branch.set_effect_root(match choice {
                            FixChoice::Left => left,
                            FixChoice::Right => right,
                        });
                    } else {
                        let root = active.root.clone();
                        let mut right_choices = active.choices.clone();
                        right_choices.push(FixChoice::Right);
                        active.choices.push(FixChoice::Left);
                        active.next_choice += 1;
                        branch.set_effect_root(left);
                        branch.fix_restarts.push(FixRestart {
                            root,
                            choices: right_choices,
                            inherited_restarts,
                        });
                    }
                    MachineWork::Drive {
                        branch,
                        scope_depth,
                    }
                } else {
                    MachineWork::Outcome {
                        outcome: BranchOutcome::Fork(
                            Box::new(branch.with_effect_root(left)),
                            Box::new(branch.with_effect_root(right)),
                        ),
                        scope_depth,
                    }
                }
            }
            Request::Fail => MachineWork::Outcome {
                outcome: branch.into_failure(),
                scope_depth,
            },
            Request::Cut(operation) => {
                return Ok(MachineStep::Continue(self.enter_cut(
                    operation,
                    branch,
                    scope_depth,
                )));
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
                    .get(&id)
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
            Request::Specialized(request, arguments) => {
                let request = self.specialization.start_request(
                    request,
                    arguments
                        .into_iter()
                        .map(PublicValue::from_runtime_root)
                        .collect(),
                );
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
        result: RequestResult,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> MachineWork<S> {
        match result {
            RequestResult::Return(value) => {
                MachineWork::deliver_root(value.into_runtime_root(), branch, scope_depth)
            }
            RequestResult::Alternatives(values) => {
                let public_values =
                    crate::api::Values::from_core_factory(self.eval_context.values().clone());
                let values = values
                    .iter()
                    .map(|value| public_values.clone_core(value))
                    .collect::<Result<Vec<_>, _>>()
                    .expect("validated specialized alternatives share the task runtime");
                match values.as_slice() {
                    [] => MachineWork::Outcome {
                        outcome: branch.into_failure(),
                        scope_depth,
                    },
                    [value] => MachineWork::deliver(
                        self.eval_context.values(),
                        value.clone(),
                        branch,
                        scope_depth,
                    ),
                    _ => MachineWork::Drive {
                        branch: branch.with_effect_root(alternative_returns_root(
                            self.eval_context.values(),
                            &self.tags,
                            values,
                        )),
                        scope_depth,
                    },
                }
            }
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
            RequestResult::ReturnUnit => MachineWork::deliver(
                self.eval_context.values(),
                self.eval_context.values().unit(),
                branch,
                scope_depth,
            ),
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
                        let diagnostic_context = evaluator.project_root(&diagnostic_context);
                        let value = evaluator.project_root(&value);
                        let assertion = evaluator.construct_lazy_value(|access| {
                            Value::builtin_call_in(
                                access,
                                Builtin::AssertUnit,
                                vec![diagnostic_context, value, self.eval_context.values().unit()],
                            )
                        });
                        evaluator.root_value(assertion)
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
                    let assignment = context.evaluate(&self.eval_context, |evaluator| {
                        evaluator.project_root(&value)
                    });
                    let published =
                        self.eval_context
                            .values()
                            .with_runtime_value_access(|access| {
                                handle.publish(&access, Ok(assignment))
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
        operation: RuntimeValueRoot,
        mut outer: Branch<S>,
        parent_scope_depth: usize,
    ) -> MachineWork<S> {
        let outer_sequence = std::mem::take(&mut outer.control.sequence);
        debug_assert_eq!(operation.runtime_id(), outer.effect.runtime_id());
        let mut frame = CutFrame {
            operation,
            outer,
            outer_sequence,
            parent_scope_depth,
            scope_depth: parent_scope_depth + 1,
            owns_transaction: false,
            alternatives: Vec::new(),
            retry: None,
            observed_failure: false,
        };
        frame.owns_transaction = frame.outer.transaction.is_none();
        self.begin_cut_attempt(&mut frame);
        let work = frame.next_alternative();
        self.execution.cuts.push(frame);
        work
    }

    fn begin_cut_attempt(&mut self, frame: &mut CutFrame<S>) {
        frame.alternatives.clear();
        frame.retry = None;
        frame.observed_failure = false;
        if frame.owns_transaction {
            let snapshot = self.host.snapshot();
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
        context: &EvaluationPollContext,
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
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            completed.transaction = None;
                        }
                        CommitResult::Conflict => {
                            let frame = self
                                .execution
                                .cuts
                                .last_mut()
                                .expect("checked nonempty cut stack");
                            frame.observed_failure = true;
                            frame.retry = Some(completed);
                            return self.finish_cut_attempt();
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
                completed.control.sequence = frame.outer_sequence;
                Ok(MachineStep::Continue(MachineWork::deliver_root(
                    value,
                    completed,
                    frame.parent_scope_depth,
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
                    return Ok(MachineStep::Continue(frame.next_alternative()));
                }
                self.finish_cut_attempt()
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

    fn finish_cut_attempt(&mut self) -> Result<MachineStep<S>, TaskHalt> {
        let mut frame = self
            .execution
            .cuts
            .pop()
            .expect("cut attempt requires a cut frame");
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
            .unwrap_or_else(|| self.host.snapshot().generation());
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
        match outcome {
            BranchOutcome::Complete(value, _) => Ok(MachineStep::Terminal(TaskTerminal::Complete(
                PublicValue::from_runtime_root(value),
            ))),
            BranchOutcome::Fail(_) => Ok(MachineStep::Terminal(TaskTerminal::Failed(
                TaskHalt::new("reflection task failed permanently"),
            ))),
            BranchOutcome::Fork(_, _) => Ok(MachineStep::Terminal(TaskTerminal::Failed(
                TaskHalt::new("`.alt` requires an enclosing `.cut`"),
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
                    .unwrap_or_else(|| self.advance_isolated_search()))
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
                Ok(self.advance_isolated_search())
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

    fn advance_isolated_search(&mut self) -> MachineStep<S> {
        if let Some(branch) = self.search.next_alternative() {
            MachineStep::Continue(MachineWork::Drive {
                branch,
                scope_depth: 0,
            })
        } else {
            self.search.finish();
            let values = crate::api::Values::from_core_factory(self.eval_context.values().clone());
            MachineStep::Terminal(TaskTerminal::Complete(values.unit()))
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

    fn waiting_block(&self, dependency: WorkDependency) -> BlockedExecution<S> {
        BlockedExecution::waiting_on(dependency, self.retry_wake())
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

    fn install_blocked(&mut self, blocked: BlockedExecution<S>) -> Option<EffectTaskPoll> {
        self.blocked = Some(blocked);
        // A resumed machine may have acquired a new transactional observation
        // after the publication which woke its previous dependency. Validate
        // the complete read set before exposing the new blocked state; no
        // later publication is required to rescue a stale subscription.
        match self.validate_blocked_retry() {
            BlockedRetryPoll::Stable => Some(self.blocked_poll()),
            BlockedRetryPoll::Restarted => None,
            BlockedRetryPoll::Terminal(poll) => Some(poll),
        }
    }

    fn validate_blocked_retry(&mut self) -> BlockedRetryPoll {
        let retry_validation = self
            .blocked
            .as_ref()
            .expect("blocked validation requires installed blocked work")
            .retry
            .as_ref()
            .and_then(|retry| {
                retry
                    .validation
                    .as_ref()
                    .map(|transaction| self.host.validate(transaction.validation()))
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
                self.apply_wake(retry.action);
                BlockedRetryPoll::Restarted
            }
            Some(ValidationResult::MissingVolume(volume)) => {
                self.finish(TaskTerminal::Failed(missing_volume_error(volume)));
                BlockedRetryPoll::Terminal(
                    self.terminal.as_ref().expect("terminal set above").poll(),
                )
            }
            Some(ValidationResult::Closed) => {
                self.finish(TaskTerminal::Cancelled);
                BlockedRetryPoll::Terminal(
                    self.terminal.as_ref().expect("terminal set above").poll(),
                )
            }
            None => {
                let changed = self.blocked.as_ref().is_some_and(|blocked| {
                    blocked.retry.as_ref().is_some_and(|retry| {
                        self.host.snapshot().generation() != retry.observed_generation
                    })
                });
                if changed {
                    let retry = self
                        .blocked
                        .take()
                        .and_then(|blocked| blocked.retry)
                        .expect("changed retry generation must retain its wake action");
                    self.apply_wake(retry.action);
                    BlockedRetryPoll::Restarted
                } else {
                    BlockedRetryPoll::Stable
                }
            }
        }
    }

    fn poll_blocked(&mut self) -> Option<EffectTaskPoll> {
        self.blocked.as_ref()?;
        match self.validate_blocked_retry() {
            BlockedRetryPoll::Stable => {}
            BlockedRetryPoll::Restarted => return None,
            BlockedRetryPoll::Terminal(poll) => return Some(poll),
        }
        let blocked = self.blocked.as_ref().expect("checked blocked state above");
        let BlockReason::WaitingOn(dependency) = &blocked.reason else {
            return Some(self.blocked_poll());
        };
        if dependency.is_terminal() {
            self.blocked = None;
            return None;
        }
        let WorkDependency::Wait(wait) = dependency else {
            return Some(self.blocked_poll());
        };
        match self.eval_context.poll_wait(wait) {
            EvaluationWaitPoll::Pending(_) => {
                if matches!(
                    self.eval_context
                        .pump_wait_on_route(wait, 256, &mut self.exact_demand_route,),
                    EvaluationPumpOutcome::TargetReady
                ) {
                    self.blocked = None;
                    None
                } else {
                    Some(self.blocked_poll())
                }
            }
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

    fn poll_exit(&mut self) -> Option<EffectTaskPoll> {
        let validation = self.exit.as_ref()?.restart.as_ref().and_then(|retry| {
            retry
                .validation
                .as_ref()
                .map(|transaction| self.host.validate(transaction.validation()))
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
                self.finish(TaskTerminal::Failed(missing_volume_error(volume)));
                return Some(self.terminal.as_ref().expect("terminal set above").poll());
            }
            Some(ValidationResult::Closed) => {
                self.finish(TaskTerminal::Cancelled);
                return Some(self.terminal.as_ref().expect("terminal set above").poll());
            }
            None => {
                let exit = self.exit.as_ref().expect("checked exit state above");
                let changed = exit.restart.as_ref().is_some_and(|retry| {
                    self.host.snapshot().generation() != retry.observed_generation
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
        self.apply_wake(retry.action);
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

    fn apply_wake(&mut self, wake: WakeAction<S>) {
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.controlling = None;
        self.execution.specializing = None;
        match wake {
            WakeAction::ReplaceWork(work) => self.execution.work = *work,
            WakeAction::RestartCut(index) => self.restart_cut(index),
            WakeAction::RestartSearch => self.restart_search(),
        }
    }

    fn restart_search(&mut self) {
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
        root.transaction = Some(Transaction::new(self.host.snapshot()));
        self.execution.work = MachineWork::Drive {
            branch: root,
            scope_depth: 0,
        };
    }

    fn restart_cut(&mut self, index: usize) {
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
        self.begin_cut_attempt(&mut frame);
        let work = frame.next_alternative();
        self.execution.cuts.push(frame);
        self.execution.work = work;
    }

    pub(super) fn finish(&mut self, terminal: TaskTerminal) {
        if self.terminal.is_some() {
            return;
        }
        let terminal = match terminal {
            TaskTerminal::Failed(error) => {
                TaskTerminal::Failed(error.root_for_values(self.eval_context.values()))
            }
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
        self.eval_context.fail_local_promises(unfinished_failure);
        self.execution.decoding = None;
        self.execution.demanding = None;
        self.execution.pathing = None;
        self.execution.controlling = None;
        self.execution.specializing = None;
        self.blocked = None;
        self.exit = None;
        self.terminal = Some(terminal);
    }
}

fn effect_dispatch_context(stage: &str) -> Value {
    let stage_key = Key::binary_from_text("stage");
    let stage = Value::Atom(Atom::from_key(&Key::binary_from_text(stage)));
    crate::diagnostic::evaluation_context_frame_with_args(
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
    pub(super) fn new(task: EffectTask<S>, context: Value) -> Self {
        let context = task
            .eval_context
            .values()
            .construct_runtime_value_root(|_| context);
        Self { task, context }
    }
}

impl<S: TaskSpecialization> EvaluationTaskMachine for ValueEffectTask<S> {
    fn poll(
        &mut self,
        context: &crate::evaluation::EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        poll_value_effect_task(&mut self.0, context, step_budget)
    }

    fn cancel(&mut self) {
        self.0.finish(TaskTerminal::Cancelled);
    }
}

impl<S: TaskSpecialization> EvaluationTaskMachine for ContextualValueEffectTask<S> {
    fn poll(
        &mut self,
        context: &crate::evaluation::EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        match poll_value_effect_task(&mut self.task, context, step_budget) {
            EvaluationMachinePoll::Failed(error) => {
                let failure = context.evaluate(&self.task.eval_context, |evaluator| {
                    error
                        .as_failure()
                        .with_context(evaluator.project_root(&self.context))
                });
                EvaluationMachinePoll::Failed(context.root_failure(Arc::new(failure)))
            }
            poll => poll,
        }
    }

    fn cancel(&mut self) {
        self.task.finish(TaskTerminal::Cancelled);
    }
}

fn poll_value_effect_task<S: TaskSpecialization>(
    task: &mut EffectTask<S>,
    context: &crate::evaluation::EvaluationPollContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> EvaluationMachinePoll {
    let observed_epoch = task.eval_context.current_observation_epoch();
    match task.poll_with_context(context, step_budget) {
        EffectTaskPoll::Yielded => EvaluationMachinePoll::Yielded,
        EffectTaskPoll::Blocked(blocked) => {
            if let Some(error) = blocked
                .dependency
                .as_ref()
                .and_then(|dependency| task.eval_context.recursive_promise_dependency(dependency))
            {
                let error = TaskHalt::new(error);
                task.finish(TaskTerminal::Failed(error.clone()));
                EvaluationMachinePoll::Failed(RuntimeFailureRoot::new(
                    task.eval_context.values(),
                    error.into_failure(),
                ))
            } else {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: blocked.dependency,
                    observed_epoch: blocked.observed_generation.map(|_| observed_epoch),
                    error: blocked.error,
                })
            }
        }
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
        context: &crate::evaluation::EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        let observed_epoch = self.0.eval_context.current_observation_epoch();
        match self.0.poll_with_context(context, step_budget) {
            EffectTaskPoll::Yielded => EvaluationMachinePoll::Yielded,
            EffectTaskPoll::Blocked(blocked) => {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: blocked.dependency,
                    observed_epoch: blocked.observed_generation.map(|_| observed_epoch),
                    error: blocked.error,
                })
            }
            EffectTaskPoll::Exit(exit) => EvaluationMachinePoll::Exit(EvaluationExitBlock {
                intent: exit.intent,
                observed_epoch: exit.observed_generation.map(|_| observed_epoch),
            }),
            EffectTaskPoll::Complete(value) => {
                let value = value.into_runtime_root();
                let (is_unit, kind) = context.evaluate(&self.0.eval_context, |evaluator| {
                    let value = evaluator.project_root(&value);
                    (
                        value == self.0.eval_context.values().unit(),
                        value.diagnostic_kind_name(),
                    )
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
        self.0.finish(TaskTerminal::Cancelled);
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
}

impl<S: TaskSpecialization> Branch<S> {
    fn new(values: &CoreValueFactory, effect: Value, state: Value) -> Self {
        let (effect, state) = values.with_runtime_value_access(|access| {
            (
                access.root_runtime_value(effect),
                access.root_runtime_value(state),
            )
        });
        Self {
            effect,
            control: Control::default(),
            state,
            transaction: None,
            active_fixes: Vec::new(),
            fix_restarts: Vec::new(),
            retry: None,
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

    fn set_state(&mut self, values: &CoreValueFactory, state: Value) {
        debug_assert_eq!(values.runtime_id(), self.state.runtime_id());
        self.state = values.construct_runtime_value_root(|_| state);
    }

    fn root_value(&self, values: &CoreValueFactory, value: Value) -> RuntimeValueRoot {
        debug_assert_eq!(values.runtime_id(), self.effect.runtime_id());
        values.construct_runtime_value_root(|_| value)
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

    fn contextualize(&self, halt: TaskHalt) -> TaskHalt {
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
        halt.with_core_context(effect_dispatch_context(stage))
    }
}

enum EffectDecodeStep<S: TaskSpecialization> {
    Continue(EffectDecodeWork<S>),
    Complete(MachineWork<S>),
    Blocked(EffectDecodeWork<S>, WorkDependency),
    Yielded(EffectDecodeWork<S>),
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
    Yielded(ScalarDemandWork<S>),
    Failed(ScalarDemandWork<S>, TaskHalt),
}

/// Sole owner of specialization-defined request progress.
///
/// The request state is never cloned into a retry checkpoint. A semantic
/// dependency retains its WHNF computation here, while an optimistic
/// transaction retry reconstructs fresh request work from the branch
/// checkpoint.
struct SpecializationWork<S: TaskSpecialization> {
    request: S::RequestWork,
    input: Option<SpecializationRequestInput>,
    demand: Option<WhnfComputation>,
    branch: Branch<S>,
    scope_depth: usize,
}

impl<S: TaskSpecialization> SpecializationWork<S> {
    fn new(request: S::RequestWork, branch: Branch<S>, scope_depth: usize) -> Self {
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
    Yielded(Box<SpecializationWork<S>>),
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
    Yielded(ControlWork<S>),
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
    VolumeGet(VolumeId),
    VolumeSet(VolumeId, RuntimeValueRoot),
    VolumeRewrite(VolumeId, RuntimeValueRoot),
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
        volume: VolumeId,
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
        volume: VolumeId,
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
        volume: VolumeId,
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
    Yielded(StatePathWork<S>),
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
            WhnfOwnerPoll::Yielded => return ValuePathPoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => {
                return ValuePathPoll::Failed(TaskHalt::rooted_failure(failure));
            }
            WhnfOwnerPoll::External(boundary) => {
                return ValuePathPoll::Failed(TaskHalt::new(format!(
                    "reflection value path reached an unsupported {boundary:?} boundary"
                )));
            }
        };
        let key = &self.path[self.next];
        let selected = poll_context.evaluate(context, |evaluator| {
            let current = evaluator.project_root(&current);
            let Value::Dict(dict) = current else {
                return Err(TaskHalt::new("state path traverses a non-dictionary value"));
            };
            Ok(evaluator.root_value(
                dict.get(key)
                    .cloned()
                    .unwrap_or_else(|| Value::Dict(Dict::new_sync())),
            ))
        });
        match selected {
            Ok(selected) => {
                self.current = selected;
                self.next += 1;
                self.demand = None;
                ValuePathPoll::Yielded
            }
            Err(error) => ValuePathPoll::Failed(error),
        }
    }
}

enum ValuePathPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    Yielded,
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
        values: &CoreValueFactory,
        value: Value,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        let value = branch.root_value(values, value);
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

    #[cfg(test)]
    fn apply(
        values: &CoreValueFactory,
        function: Value,
        arguments: Vec<Value>,
        branch: Branch<S>,
        scope_depth: usize,
    ) -> Self {
        let function = branch.root_value(values, function);
        let arguments = arguments
            .into_iter()
            .map(|argument| branch.root_value(values, argument))
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
        let value = branch.root_value(values, value);
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
    retry: Option<Branch<S>>,
    observed_failure: bool,
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
        assert!(
            error.blocked_on().is_none(),
            "a blocked task error belongs in the wait dependency field"
        );
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

pub(super) enum EffectTaskPoll {
    Yielded,
    Blocked(TaskBlock),
    Exit(TaskExitBlock),
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
    Fix(V),
    Get(V),
    Set(V, V),
    HeapGet(V),
    HeapSet(V, V),
    HeapRewrite(V, V),
    VolumeGet(VolumeId, V),
    VolumeSet(VolumeId, V, V),
    VolumeRewrite(VolumeId, V, V),
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VolumeRequestIdentity {
    volume: VolumeId,
    operation: VolumeOperation,
}

enum RequestSelection<R> {
    Return,
    Seq,
    Alt,
    Fail,
    Cut,
    Fix,
    Get,
    Set,
    HeapGet,
    HeapSet,
    HeapRewrite,
    Volume(VolumeRequestIdentity),
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
            Self::Fail | Self::ExitSuccess => 0,
            Self::Return
            | Self::Cut
            | Self::Fix
            | Self::Get
            | Self::HeapGet
            | Self::ExitError
            | Self::Volume(VolumeRequestIdentity {
                operation: VolumeOperation::Get,
                ..
            }) => 1,
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
            Self::Fix => Request::Fix(next()),
            Self::Get => Request::Get(next()),
            Self::Set => Request::Set(next(), next()),
            Self::HeapGet => Request::HeapGet(next()),
            Self::HeapSet => Request::HeapSet(next(), next()),
            Self::HeapRewrite => Request::HeapRewrite(next(), next()),
            Self::Volume(VolumeRequestIdentity {
                volume,
                operation: VolumeOperation::Get,
            }) => Request::VolumeGet(volume, next()),
            Self::Volume(VolumeRequestIdentity {
                volume,
                operation: VolumeOperation::Set,
            }) => Request::VolumeSet(volume, next(), next()),
            Self::Volume(VolumeRequestIdentity {
                volume,
                operation: VolumeOperation::Rewrite,
            }) => Request::VolumeRewrite(volume, next(), next()),
            Self::Reset => Request::Reset(next(), next()),
            Self::Shift => Request::Shift(next(), next()),
            Self::ExitSuccess => Request::ExitSuccess,
            Self::ExitError => Request::ExitError(next()),
            Self::Specialized { request, .. } => Request::Specialized(request, arguments.collect()),
            Self::Resume => unreachable!("resume IDs require WHNF conversion before dispatch"),
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
    Poisoned,
}

enum RequestDecodePoll<R> {
    Ready(Request<R>),
    Continue,
    Pending(WorkDependency),
    Yielded,
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
                    select_request_from_whnf(
                        evaluator,
                        evaluator.project_root(request),
                        tags,
                        specialized,
                    )
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
                let payload = match poll_whnf_computation(
                    demand,
                    poll_context,
                    context,
                    step_budget,
                ) {
                    WhnfOwnerPoll::Ready(payload) => payload,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return RequestDecodePoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return RequestDecodePoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => {
                        return RequestDecodePoll::Failed(TaskHalt::rooted_failure(failure));
                    }
                    WhnfOwnerPoll::External(boundary) => {
                        return RequestDecodePoll::Failed(TaskHalt::new(format!(
                            "request payload decoding reached an unsupported {boundary:?} boundary"
                        )));
                    }
                };
                let is_list = poll_context.evaluate(context, |evaluator| {
                    matches!(evaluator.project_root(&payload), Value::List(_))
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
                    eval::ListFrontPoll::Yielded => RequestDecodePoll::Yielded,
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
                    RequestIdPoll::Yielded => return RequestDecodePoll::Yielded,
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
                    RequestIdPoll::Yielded => RequestDecodePoll::Yielded,
                    RequestIdPoll::Failed(error) => RequestDecodePoll::Failed(error),
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
    Yielded,
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
        WhnfOwnerPoll::Yielded => return RequestIdPoll::Yielded,
        WhnfOwnerPoll::Failed(failure) => {
            return RequestIdPoll::Failed(TaskHalt::rooted_failure(failure));
        }
        WhnfOwnerPoll::External(boundary) => {
            return RequestIdPoll::Failed(TaskHalt::new(format!(
                "request ID decoding reached an unsupported {boundary:?} boundary"
            )));
        }
    };
    poll_context.evaluate(context, |evaluator| {
        let Value::Number(value) = evaluator.project_root(&value) else {
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
}

fn select_request_from_whnf<R: Clone>(
    context: &EvaluatorStepContext<'_>,
    value: Value,
    tags: &Tags,
    specialized: &[SpecializedRequest<R>],
) -> Result<(RequestSelection<R>, RuntimeValueRoot), TaskHalt> {
    let Value::Dict(dict) = value else {
        return Err(TaskHalt::new("effect API returned a non-request value"));
    };
    let selected =
        |selection, payload: &Value| Ok((selection, context.root_value(payload.clone())));
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
    select!(&tags.fix, RequestSelection::Fix);
    select!(&tags.get, RequestSelection::Get);
    select!(&tags.set, RequestSelection::Set);
    select!(&tags.heap_get, RequestSelection::HeapGet);
    select!(&tags.heap_set, RequestSelection::HeapSet);
    select!(&tags.heap_rewrite, RequestSelection::HeapRewrite);
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
            ]
            .into_iter()
            .fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key, value)
            }),
        );
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
                "alt",
                request_function_in(access, tags.alt.clone(), 2, Vec::new(), false),
            ),
            entry("fail", nullary_request(tags.fail.clone())),
            entry(
                "cut",
                request_function_in(access, tags.cut.clone(), 1, Vec::new(), false),
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
                "reset",
                request_function_in(access, tags.reset.clone(), 2, Vec::new(), false),
            ),
            entry(
                "shift",
                request_function_in(access, tags.shift.clone(), 2, Vec::new(), false),
            ),
        ];
        if expose_shared_heap {
            entries.push(entry("heap", heap_api));
        }
        if expose_exit {
            entries.push(entry(
                "exit",
                Value::Dict(
                    [
                        entry("success", nullary_request(tags.exit_success.clone())),
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
                    nullary_request(tag.clone())
                } else {
                    request_function_in(access, tag.clone(), spec.arity, Vec::new(), false)
                };
                api = insert_effect_api_path(
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

        let nested = match current.get(&key) {
            Some(Value::Dict(nested)) => nested.clone(),
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
                .construct_managed_core_net(template.instantiate())
                .expect("managed core-net representation must fit one collector run"),
        ),
        remaining,
    ))
}

fn nullary_request(tag: Key) -> Value {
    Value::Dict(Dict::new_sync().insert(tag, Value::List(List::empty())))
}

fn alternative_returns_root(
    factory: &CoreValueFactory,
    tags: &Tags,
    values: Vec<Value>,
) -> RuntimeValueRoot {
    factory.construct_runtime_value_root(|access| {
        values
            .into_iter()
            .rev()
            .map(|value| eval::constant_effect_in(access, request_value(&tags.r, vec![value])))
            .reduce(|right, left| {
                eval::constant_effect_in(access, request_value(&tags.alt, vec![left, right]))
            })
            .expect("alternative return construction requires at least two values")
    })
}

pub(crate) fn task_eval_error(error: EvaluationHalt) -> TaskHalt {
    match error.blocked_on() {
        Some(wait) => TaskHalt::blocked(wait.0),
        None => TaskHalt::failure(error.into_permanent_failure()),
    }
}

fn missing_volume_error(volume: VolumeId) -> TaskHalt {
    TaskHalt::new(format!(
        "reflection volume {} was revoked before its edits committed",
        volume.get()
    ))
}

fn lazy_value_path_root(context: &EvalContext, value: Value, path: &[Key]) -> RuntimeValueRoot {
    context.values().construct_runtime_value_root(|access| {
        if path.is_empty() {
            return value;
        }
        Value::Lazy(LazyValue::from_access_in(
            access,
            Arc::from(
                path.iter()
                    .cloned()
                    .map(CoreDataKey::Key)
                    .collect::<Vec<_>>(),
            ),
            Arc::from([value]),
        ))
    })
}

fn reset_stack_root_in(
    context: &EvaluatorStepContext<'_>,
    state: &RuntimeValueRoot,
    continuation_state: &Key,
) -> Result<RuntimeValueRoot, TaskHalt> {
    let Value::Dict(state) = context.project_root(state) else {
        return Err(TaskHalt::new("reflection user state must be a dictionary"));
    };
    Ok(context.root_value(
        state
            .get(continuation_state)
            .cloned()
            .unwrap_or_else(|| Value::List(List::empty())),
    ))
}

fn encode_reset_frames(context: &EvaluatorStepContext<'_>, frames: &[ResetFrame]) -> Value {
    Value::List(List::from_values(
        frames
            .iter()
            .map(|frame| {
                Value::List(List::from_values(vec![
                    frame.key.to_value_with(context.context().values()),
                    context.project_root(&frame.continuation),
                    Value::Number(Number::from_usize(frame.scope_depth)),
                    Value::Number(Number::from_usize(frame.order)),
                ]))
            })
            .collect(),
    ))
}

fn encode_reset_frames_in_state(
    context: &EvaluatorStepContext<'_>,
    state: Value,
    continuation_state: &Key,
    frames: &[ResetFrame],
) -> Value {
    let Value::Dict(state) = state else {
        return context.construct_lazy_value(|access| {
            Value::Lazy(LazyValue::error_in(
                access,
                "reflection user state must remain a dictionary",
            ))
        });
    };
    Value::Dict(state.insert(
        continuation_state.clone(),
        encode_reset_frames(context, frames),
    ))
}

#[cfg(test)]
mod tests;
