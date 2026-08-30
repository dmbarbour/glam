use std::collections::HashMap;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::protocol::{
    CommitResult, EffectRequestSpec, RequestActivity, RequestContext, RequestResult, TaskCommit,
    TaskHalt, TaskHost, TaskOutcome, TaskSpecialization, Transaction, request_value,
};
use super::search::{IsolatedSearchBranch, SearchPolicy};
use super::store::{StoreJournal, VolumeId};
use crate::api::Value as PublicValue;
use crate::core::{
    Atom, Builtin, CoreValueFactory, Dict, EvaluationFailure, EvaluationHalt, FunctionValue, Key,
    LazyValue, List, NetValue, PromisedValue, Value, keys,
};
use crate::core_net::{CoreDataKey, CoreSpecialization};
use crate::eval;
#[cfg(test)]
use crate::evaluation::OwnedEvalContext;
use crate::evaluation::{
    EvalContext, EvaluationExitBlock, EvaluationMachinePoll, EvaluationPollContext,
    EvaluationPumpOutcome, EvaluationSession, EvaluationTaskBlock, EvaluationTaskId,
    EvaluationTaskMachine, EvaluationWaitPoll, EvaluationWaitToken, EvaluatorStepContext,
    ExitIntent, WorkDependency,
};
use crate::interaction_net::NetBuilder;
use crate::number::Number;
use crate::runtime::RuntimeValueRoot;

#[cfg(test)]
use super::protocol::HostSnapshot;

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
    api: Value,
    next_continuation: u64,
    next_control_order: usize,
    continuations: HashMap<u64, CapturedContinuation>,
    search: SearchPolicy<Branch<S>, IsolatedSearchBranch<S>>,
    execution: TaskExecution<S>,
    blocked: Option<BlockedExecution<S>>,
    exit: Option<TaskExitState<S>>,
    terminal: Option<TaskTerminal>,
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
    request_roots: AtomicUsize,
    fused_requests: AtomicUsize,
}

#[cfg(test)]
impl EffectPhaseProbe {
    fn record(&self, phase: EffectMachinePhase) {
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

    fn record_request_root(&self) {
        self.request_roots.fetch_add(1, Ordering::AcqRel);
    }

    fn record_fused_request(&self) {
        self.fused_requests.fetch_add(1, Ordering::AcqRel);
    }

    fn request_roots(&self) -> usize {
        self.request_roots.load(Ordering::Acquire)
    }

    fn fused_requests(&self) -> usize {
        self.fused_requests.load(Ordering::Acquire)
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
            &tags,
            specialization.requests(),
            specialization.exposes_shared_heap(),
            exposes_exit,
        )?;
        let id = eval_context
            .task_id()
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
        let initial_state = Value::Dict(Dict::new_sync());
        let root = Branch::new(effect, initial_state);
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
                cuts: Vec::new(),
            },
            blocked: None,
            exit: None,
            terminal: None,
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

    fn root_request_value(
        &self,
        context: &EvaluatorStepContext<'_>,
        value: Value,
    ) -> RuntimeValueRoot {
        #[cfg(test)]
        if let Some(probe) = &self.phase_probe {
            probe.record_request_root();
        }
        context.root_value(value)
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
        self.execution
            .work
            .branch_mut()
            .expect("a fresh effect task must contain its initial branch")
            .control
            .sequence
            .push(Continuation::AssertUnit(Value::binary_from_text(
                &diagnostic_context,
            )));
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
    ) -> Result<Value, TaskHalt> {
        let id = self.next_continuation;
        self.next_continuation = self
            .next_continuation
            .checked_add(1)
            .ok_or_else(|| TaskHalt::new("reflection continuation IDs exhausted"))?;
        self.continuations.insert(id, continuation);
        Ok(request_function(
            self.tags.resume.clone(),
            3,
            vec![
                Value::Number(Number::from_u64(self.id.get())),
                Value::Number(Number::from_u64(id)),
            ],
            true,
        ))
    }

    fn start_fixpoint(
        &mut self,
        context: &EvaluationPollContext,
        root: Arc<FixRoot<S>>,
        choices: Vec<FixChoice>,
    ) -> Result<MachineWork<S>, TaskHalt> {
        let mut branch = root.entry.clone();
        let (reset_stack, state) = context.evaluate(&self.eval_context, |evaluator| {
            Ok::<_, TaskHalt>((
                evaluator.root_value(reset_stack_value_in(
                    evaluator,
                    &branch.state,
                    &self.tags.continuation_state,
                )?),
                evaluator.root_value(with_reset_frames_in(
                    evaluator,
                    branch.state.clone(),
                    &self.tags.continuation_state,
                    &[],
                )?),
            ))
        })?;
        let reset_stack = reset_stack.into_core();
        let state = state.into_core();
        let order = self.allocate_control_order()?;
        let handle = PromisedValue::fixpoint(&self.eval_context, "reflection effect fixpoint")
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
        let marker = Value::Promised(handle.clone());
        let outer_control = std::mem::take(&mut branch.control);
        branch.state = state;
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
        Ok(MachineWork::Apply {
            function: root.function.clone(),
            arguments: vec![marker],
            branch,
            scope_depth: root.scope_depth,
        })
    }

    fn restart_fixpoint_at_scope(
        &mut self,
        context: &EvaluationPollContext,
        branch: &mut Branch<S>,
        scope_depth: usize,
    ) -> Result<Option<MachineWork<S>>, TaskHalt> {
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
        let mut restarted = self.start_fixpoint(context, restart.root, restart.choices)?;
        restarted
            .branch_mut()
            .expect("fixpoint restart must retain its branch")
            .fix_restarts = restart.inherited_restarts;
        Ok(Some(restarted))
    }

    fn install_captured_control(
        &mut self,
        context: &EvaluationPollContext,
        branch: &mut Branch<S>,
        captured: &CapturedContinuation,
        scope_depth: usize,
    ) -> Result<(), TaskHalt> {
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

        let mut reset_frames = context.evaluate(&self.eval_context, |evaluator| {
            reset_frames_in(evaluator, &branch.state, &self.tags.continuation_state)
        })?;
        let next_order = self
            .next_control_order
            .checked_add(layers.len())
            .ok_or_else(|| TaskHalt::new("reflection control order exhausted"))?;
        let mut delimiters = Vec::new();
        for (order, layer) in (self.next_control_order..).zip(layers) {
            match layer {
                CapturedLayer::Reset(mut frame) => {
                    frame.scope_depth = scope_depth;
                    frame.order = order;
                    reset_frames.push(frame);
                }
                CapturedLayer::Delimiter(mut delimiter) => {
                    delimiter.rebase(scope_depth, order);
                    delimiters.push(delimiter);
                }
            }
        }
        let state = context
            .evaluate(&self.eval_context, |evaluator| {
                with_reset_frames_in(
                    evaluator,
                    branch.state.clone(),
                    &self.tags.continuation_state,
                    &reset_frames,
                )
                .map(|state| evaluator.root_value(state))
            })?
            .into_core();
        self.next_control_order = next_order;
        branch.state = state;
        branch.control.delimiters.extend(delimiters);
        Ok(())
    }

    pub(super) fn run(&mut self) -> Result<TaskOutcome, TaskHalt> {
        loop {
            match self.poll(256) {
                EffectTaskPoll::Yielded => {}
                EffectTaskPoll::Blocked(blocked) => {
                    if let Some(wait) = blocked.lazy {
                        match self.eval_context.pump_wait(&wait, 4_096) {
                            EvaluationPumpOutcome::TargetReady
                            | EvaluationPumpOutcome::BudgetExhausted => continue,
                            EvaluationPumpOutcome::Busy => {
                                self.eval_context.wait_for_claimed_task(&wait);
                                continue;
                            }
                            EvaluationPumpOutcome::NoProgress
                                if blocked.observed_generation.is_none() =>
                            {
                                if self
                                    .eval_context
                                    .wait_for_observed_dependency_progress(&wait)
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
        self.poll_with_context(&context, steps)
    }

    pub(super) fn poll_with_context(
        &mut self,
        context: &EvaluationPollContext,
        steps: usize,
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

        for _ in 0..steps {
            let work = self.execution.work.clone();
            match self.step(context, work) {
                Ok(MachineStep::Continue(work)) => self.execution.work = work,
                Ok(MachineStep::Blocked(blocked)) => {
                    self.blocked = Some(blocked);
                    return self.blocked_poll();
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
                Err(error) => {
                    if let Some(wait) = error.blocked_on() {
                        self.blocked = Some(self.waiting_block(wait.clone()));
                        return self.blocked_poll();
                    }
                    if let Some(retry) = self.retry_wake() {
                        self.blocked = Some(BlockedExecution::evaluation_error(error, retry));
                        return self.blocked_poll();
                    }
                    self.finish(TaskTerminal::Failed(error));
                    return self.terminal.as_ref().expect("terminal set above").poll();
                }
            }
        }
        EffectTaskPoll::Yielded
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
                mut branch,
                scope_depth,
            } => {
                branch.effect = context
                    .evaluate(&self.eval_context, |evaluator| {
                        apply_in(evaluator, function, arguments)
                            .map(|value| evaluator.root_value(value))
                    })?
                    .into_core();
                #[cfg(test)]
                self.record_phase(EffectMachinePhase::ContinuationDelivered);
                Ok(MachineStep::Continue(MachineWork::Drive {
                    branch,
                    scope_depth,
                }))
            }
            MachineWork::Outcome {
                outcome,
                scope_depth,
            } => self.handle_outcome(context, outcome, scope_depth),
        }
    }

    fn prepare_drive_in(
        &self,
        context: &EvaluatorStepContext<'_>,
        branch: &mut Branch<S>,
    ) -> Result<PreparedDrive<S::Request>, TaskHalt> {
        if !self.fusion_enabled() {
            return self
                .effect_request_in(context, branch.effect.clone())
                .map(|request| PreparedDrive::Request {
                    request,
                    _phase_roots: Vec::new(),
                });
        }

        let mut fusion = FusionState::new(branch.control.sequence.len());

        for _ in 0..EFFECT_FUSION_BUDGET {
            let request = self.effect_request_values_in(context, branch.effect.clone())?;
            match self.classify_fused_request(branch, request, &mut fusion) {
                FusedRequestAction::Continue => continue,
                FusedRequestAction::Deliver(value) => {
                    return self.finish_fused_delivery_in(context, branch, value, fusion);
                }
                FusedRequestAction::Get(path) => {
                    let path =
                        eval::eval_key_path_list_in(context, &path).map_err(task_eval_error)?;
                    let value = get_value_path_in(context, &branch.state, &path)?;
                    return self.finish_fused_delivery_in(context, branch, value, fusion);
                }
                FusedRequestAction::Set(path, value) => {
                    branch.state = set_state_path_in(context, branch.state.clone(), &path, value)?;
                    fusion.state_dirty = true;
                    return self.finish_fused_delivery_in(
                        context,
                        branch,
                        self.eval_context.values().unit(),
                        fusion,
                    );
                }
                FusedRequestAction::Boundary(request) => {
                    return Ok(self.finish_fused_request(
                        context,
                        branch,
                        request,
                        fusion.pending_sequence_values,
                        fusion.effect_dirty,
                        fusion.state_dirty,
                    ));
                }
            }
        }

        let mut phase_roots = self.root_fused_branch_updates(
            context,
            branch,
            fusion.pending_sequence_values,
            false,
            fusion.state_dirty,
        );
        let effect = self.root_request_value(context, branch.effect.clone());
        branch.effect = effect.as_core().clone();
        phase_roots.push(effect);
        Ok(PreparedDrive::Continue {
            _phase_roots: phase_roots,
        })
    }

    fn classify_fused_request(
        &self,
        branch: &mut Branch<S>,
        request: Request<S::Request, Value>,
        fusion: &mut FusionState,
    ) -> FusedRequestAction<S::Request> {
        match request {
            Request::Seq(operation, continuation) => {
                self.record_fused_request();
                fusion.pending_sequence_values.push(continuation.clone());
                branch
                    .control
                    .sequence
                    .push(Continuation::Glam(continuation));
                branch.effect = operation;
                fusion.effect_dirty = true;
                FusedRequestAction::Continue
            }
            Request::Return(value) => FusedRequestAction::Deliver(value),
            Request::Get(path) => FusedRequestAction::Get(path),
            Request::Set(path, value) => FusedRequestAction::Set(path, value),
            request => FusedRequestAction::Boundary(request),
        }
    }

    fn finish_fused_delivery_in(
        &self,
        context: &EvaluatorStepContext<'_>,
        branch: &mut Branch<S>,
        value: Value,
        mut fusion: FusionState,
    ) -> Result<PreparedDrive<S::Request>, TaskHalt> {
        if let Some(effect) = fuse_glam_delivery_in(
            context,
            branch,
            value.clone(),
            fusion.initial_sequence_depth,
            &mut fusion.pending_sequence_values,
        )? {
            self.record_fused_request();
            return Ok(self.finish_fused_continue(
                context,
                branch,
                effect,
                fusion.pending_sequence_values,
                fusion.state_dirty,
            ));
        }
        Ok(self.finish_fused_request(
            context,
            branch,
            Request::Return(value),
            fusion.pending_sequence_values,
            fusion.effect_dirty,
            fusion.state_dirty,
        ))
    }

    fn finish_fused_request(
        &self,
        context: &EvaluatorStepContext<'_>,
        branch: &Branch<S>,
        request: Request<S::Request, Value>,
        pending_sequence_values: Vec<Value>,
        effect_dirty: bool,
        state_dirty: bool,
    ) -> PreparedDrive<S::Request> {
        let phase_roots = self.root_fused_branch_updates(
            context,
            branch,
            pending_sequence_values,
            effect_dirty,
            state_dirty,
        );
        PreparedDrive::Request {
            request: request.map_values(|value| self.root_request_value(context, value)),
            _phase_roots: phase_roots,
        }
    }

    fn finish_fused_continue(
        &self,
        context: &EvaluatorStepContext<'_>,
        branch: &mut Branch<S>,
        effect: Value,
        pending_sequence_values: Vec<Value>,
        state_dirty: bool,
    ) -> PreparedDrive<S::Request> {
        let mut phase_roots = self.root_fused_branch_updates(
            context,
            branch,
            pending_sequence_values,
            false,
            state_dirty,
        );
        let effect = self.root_request_value(context, effect);
        branch.effect = effect.as_core().clone();
        phase_roots.push(effect);
        PreparedDrive::Continue {
            _phase_roots: phase_roots,
        }
    }

    fn root_fused_branch_updates(
        &self,
        context: &EvaluatorStepContext<'_>,
        branch: &Branch<S>,
        pending_sequence_values: Vec<Value>,
        effect_dirty: bool,
        state_dirty: bool,
    ) -> Vec<RuntimeValueRoot> {
        let mut roots = pending_sequence_values
            .into_iter()
            .map(|value| self.root_request_value(context, value))
            .collect::<Vec<_>>();
        if effect_dirty {
            roots.push(self.root_request_value(context, branch.effect.clone()));
        }
        if state_dirty {
            roots.push(self.root_request_value(context, branch.state.clone()));
        }
        roots
    }

    fn drive_step(
        &mut self,
        context: &EvaluationPollContext,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        let prepared = self.prepare_drive(context, &mut branch)?;
        #[cfg(test)]
        self.record_phase(EffectMachinePhase::RequestParsed);
        self.interpret_prepared_drive(context, prepared, branch, scope_depth)
    }

    fn prepare_drive(
        &self,
        context: &EvaluationPollContext,
        branch: &mut Branch<S>,
    ) -> Result<PreparedDrive<S::Request>, TaskHalt> {
        context.evaluate(&self.eval_context, |evaluator| {
            self.prepare_drive_in(evaluator, branch)
        })
    }

    fn interpret_prepared_drive(
        &mut self,
        context: &EvaluationPollContext,
        prepared: PreparedDrive<S::Request>,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        #[cfg(test)]
        self.record_phase(EffectMachinePhase::InterpreterEntered);
        let (request, _phase_roots) = match prepared {
            PreparedDrive::Request {
                request,
                _phase_roots,
            } => (request, _phase_roots),
            PreparedDrive::Continue { _phase_roots: _ } => {
                #[cfg(test)]
                self.record_phase(EffectMachinePhase::ContinuationDelivered);
                return Ok(MachineStep::Continue(MachineWork::Drive {
                    branch,
                    scope_depth,
                }));
            }
        };
        let work = match request {
            Request::Return(value) => MachineWork::Deliver {
                value: value.into_core(),
                branch,
                scope_depth,
            },
            Request::Seq(operation, continuation) => {
                branch
                    .control
                    .sequence
                    .push(Continuation::Glam(continuation.into_core()));
                branch.effect = operation.into_core();
                MachineWork::Drive {
                    branch,
                    scope_depth,
                }
            }
            Request::Alt(left, right) => {
                let left = left.into_core();
                let right = right.into_core();
                if (scope_depth > 0 || self.search.retains_all()) && !branch.active_fixes.is_empty()
                {
                    let inherited_restarts = branch.fix_restarts.clone();
                    let active = branch
                        .active_fixes
                        .first_mut()
                        .expect("checked nonempty fixpoint stack");
                    if let Some(choice) = active.choices.get(active.next_choice).copied() {
                        active.next_choice += 1;
                        branch.effect = match choice {
                            FixChoice::Left => left,
                            FixChoice::Right => right,
                        };
                    } else {
                        let root = active.root.clone();
                        let mut right_choices = active.choices.clone();
                        right_choices.push(FixChoice::Right);
                        active.choices.push(FixChoice::Left);
                        active.next_choice += 1;
                        branch.effect = left;
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
                            Box::new(branch.with_effect(left)),
                            Box::new(branch.with_effect(right)),
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
                    operation.into_core(),
                    branch,
                    scope_depth,
                )));
            }
            Request::Get(path) => {
                let value = context
                    .evaluate(&self.eval_context, |evaluator| {
                        let path = eval::eval_key_path_list_in(evaluator, path.as_core())
                            .map_err(task_eval_error)?;
                        get_value_path_in(evaluator, &branch.state, &path)
                            .map(|value| evaluator.root_value(value))
                    })?
                    .into_core();
                MachineWork::Deliver {
                    value,
                    branch,
                    scope_depth,
                }
            }
            Request::Set(path, value) => {
                branch.state = context
                    .evaluate(&self.eval_context, |evaluator| {
                        set_state_path_in(
                            evaluator,
                            branch.state,
                            path.as_core(),
                            value.into_core(),
                        )
                        .map(|state| evaluator.root_value(state))
                    })?
                    .into_core();
                MachineWork::Deliver {
                    value: self.eval_context.values().unit(),
                    branch,
                    scope_depth,
                }
            }
            Request::HeapGet(path) => {
                let path = context.evaluate(&self.eval_context, |evaluator| {
                    eval::eval_key_path_list_in(evaluator, path.as_core()).map_err(task_eval_error)
                })?;
                let checkpoint = branch.retry_candidate();
                let heap = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let observed = transaction.store.observe_read(&path);
                    let heap = transaction.store.view().as_core().clone();
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    heap
                } else {
                    let snapshot = self.host.snapshot();
                    branch.observe(checkpoint, snapshot.generation());
                    snapshot.store().root().as_core().clone()
                };
                let value = lazy_value_path(&self.eval_context, heap, &path);
                MachineWork::Deliver {
                    value,
                    branch,
                    scope_depth,
                }
            }
            Request::HeapSet(path, value) => {
                let path = context.evaluate(&self.eval_context, |evaluator| {
                    eval::eval_key_path_list_in(evaluator, path.as_core()).map_err(task_eval_error)
                })?;
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction
                        .store
                        .write(path, PublicValue::from_runtime_root(value));
                    MachineWork::Deliver {
                        value: self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    }
                } else {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.write(path, PublicValue::from_runtime_root(value));
                    let commit =
                        TaskCommit::new(store, snapshot.extra().clone(), S::Journal::default());
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            branch.retry = None;
                            MachineWork::Deliver {
                                value: self.eval_context.values().unit(),
                                branch,
                                scope_depth,
                            }
                        }
                        CommitResult::Conflict => MachineWork::Drive {
                            branch,
                            scope_depth,
                        },
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => MachineWork::Outcome {
                            outcome: BranchOutcome::Cancelled,
                            scope_depth,
                        },
                    }
                }
            }
            Request::HeapRewrite(path, updater) => {
                let path = context.evaluate(&self.eval_context, |evaluator| {
                    eval::eval_key_path_list_in(evaluator, path.as_core()).map_err(task_eval_error)
                })?;
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction
                        .store
                        .rewrite(path, PublicValue::from_runtime_root(updater));
                    MachineWork::Deliver {
                        value: self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    }
                } else {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.rewrite(path, PublicValue::from_runtime_root(updater));
                    let commit =
                        TaskCommit::new(store, snapshot.extra().clone(), S::Journal::default());
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            branch.retry = None;
                            MachineWork::Deliver {
                                value: self.eval_context.values().unit(),
                                branch,
                                scope_depth,
                            }
                        }
                        CommitResult::Conflict => MachineWork::Drive {
                            branch,
                            scope_depth,
                        },
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => MachineWork::Outcome {
                            outcome: BranchOutcome::Cancelled,
                            scope_depth,
                        },
                    }
                }
            }
            Request::VolumeGet(volume, path) => {
                let path = context.evaluate(&self.eval_context, |evaluator| {
                    eval::eval_key_path_list_in(evaluator, path.as_core()).map_err(task_eval_error)
                })?;
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
                    branch.observe(checkpoint, snapshot.generation());
                    snapshot.store().volume(volume).cloned()
                };
                let value = match root {
                    Some(root) => lazy_value_path(&self.eval_context, root.into_core(), &path),
                    None => missing_volume_value(&self.eval_context, volume),
                };
                MachineWork::Deliver {
                    value,
                    branch,
                    scope_depth,
                }
            }
            Request::VolumeSet(volume, path, value) => {
                let path = context.evaluate(&self.eval_context, |evaluator| {
                    eval::eval_key_path_list_in(evaluator, path.as_core()).map_err(task_eval_error)
                })?;
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction.store.write_volume(
                        volume,
                        path,
                        PublicValue::from_runtime_root(value),
                    );
                    MachineWork::Deliver {
                        value: self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    }
                } else {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.write_volume(volume, path, PublicValue::from_runtime_root(value));
                    let commit =
                        TaskCommit::new(store, snapshot.extra().clone(), S::Journal::default());
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            branch.retry = None;
                            MachineWork::Deliver {
                                value: self.eval_context.values().unit(),
                                branch,
                                scope_depth,
                            }
                        }
                        CommitResult::Conflict => MachineWork::Drive {
                            branch,
                            scope_depth,
                        },
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => MachineWork::Outcome {
                            outcome: BranchOutcome::Cancelled,
                            scope_depth,
                        },
                    }
                }
            }
            Request::VolumeRewrite(volume, path, updater) => {
                let path = context.evaluate(&self.eval_context, |evaluator| {
                    eval::eval_key_path_list_in(evaluator, path.as_core()).map_err(task_eval_error)
                })?;
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction.store.rewrite_volume(
                        volume,
                        path,
                        PublicValue::from_runtime_root(updater),
                    );
                    MachineWork::Deliver {
                        value: self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    }
                } else {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.rewrite_volume(volume, path, PublicValue::from_runtime_root(updater));
                    let commit =
                        TaskCommit::new(store, snapshot.extra().clone(), S::Journal::default());
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            branch.retry = None;
                            MachineWork::Deliver {
                                value: self.eval_context.values().unit(),
                                branch,
                                scope_depth,
                            }
                        }
                        CommitResult::Conflict => MachineWork::Drive {
                            branch,
                            scope_depth,
                        },
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => MachineWork::Outcome {
                            outcome: BranchOutcome::Cancelled,
                            scope_depth,
                        },
                    }
                }
            }
            Request::Reset(key, operation) => {
                let (key, mut frames) = context.evaluate(&self.eval_context, |evaluator| {
                    Ok::<_, TaskHalt>((
                        value_key_in(evaluator, key.into_core())?,
                        reset_frames_in(evaluator, &branch.state, &self.tags.continuation_state)?,
                    ))
                })?;
                let order = self.allocate_control_order()?;
                let continuation = self.capture_continuation(CapturedContinuation {
                    sequence: std::mem::take(&mut branch.control.sequence),
                    delimiters: Vec::new(),
                    reset_frames: Vec::new(),
                })?;
                frames.push(ResetFrame {
                    key,
                    continuation,
                    scope_depth,
                    order,
                });
                branch.state = replace_reset_frames(
                    &self.eval_context,
                    branch.state,
                    &self.tags.continuation_state,
                    &frames,
                );
                branch.effect = operation.into_core();
                MachineWork::Drive {
                    branch,
                    scope_depth,
                }
            }
            Request::Shift(key, function) => {
                let (key, mut frames) = context.evaluate(&self.eval_context, |evaluator| {
                    Ok::<_, TaskHalt>((
                        value_key_in(evaluator, key.into_core())?,
                        reset_frames_in(evaluator, &branch.state, &self.tags.continuation_state)?,
                    ))
                })?;
                let Some(index) = frames.iter().rposition(|frame| frame.key == key) else {
                    return Err(TaskHalt::new("`.shift` key is not in reset scope"));
                };
                let inner_reset_frames = frames.split_off(index + 1);
                let target = frames.pop().expect("matching reset frame must exist");
                let first_inner_delimiter = branch
                    .control
                    .delimiters
                    .iter()
                    .position(|delimiter| delimiter.order() > target.order)
                    .unwrap_or(branch.control.delimiters.len());
                let inner_delimiters = branch.control.delimiters.split_off(first_inner_delimiter);
                let continuation = self.capture_continuation(CapturedContinuation {
                    sequence: std::mem::take(&mut branch.control.sequence),
                    delimiters: inner_delimiters,
                    reset_frames: inner_reset_frames,
                })?;
                branch.state = replace_reset_frames(
                    &self.eval_context,
                    branch.state,
                    &self.tags.continuation_state,
                    &frames,
                );
                branch
                    .control
                    .sequence
                    .push(Continuation::Glam(target.continuation));
                MachineWork::Apply {
                    function: function.into_core(),
                    arguments: vec![continuation],
                    branch,
                    scope_depth,
                }
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
                let order = self.allocate_control_order()?;
                let caller_sequence = std::mem::take(&mut branch.control.sequence);
                branch.control.delimiters.push(Delimiter::Resume {
                    outer_sequence: caller_sequence,
                    scope_depth,
                    order,
                });
                self.install_captured_control(context, &mut branch, &captured, scope_depth)?;
                branch.control.sequence = captured.sequence.clone();
                MachineWork::Deliver {
                    value: value.into_core(),
                    branch,
                    scope_depth,
                }
            }
            Request::ExitSuccess => {
                return Ok(MachineStep::Exit(ExitIntent::Success));
            }
            Request::ExitError(message) => {
                let message = context.evaluate(&self.eval_context, |evaluator| {
                    evaluate_in(evaluator, message.into_core())
                        .map(|message| evaluator.root_value(message))
                })?;
                return Ok(MachineStep::Exit(ExitIntent::Error(message)));
            }
            Request::Fix(function) => {
                let root = Arc::new(FixRoot {
                    function: function.into_core(),
                    entry: branch,
                    scope_depth,
                });
                self.start_fixpoint(context, root, Vec::new())?
            }
            Request::Specialized(request, arguments) => {
                let checkpoint = branch.retry_candidate();
                let mut activity = RequestActivity::default();
                let result = self.specialization.handle_request(
                    request,
                    arguments
                        .into_iter()
                        .map(PublicValue::from_runtime_root)
                        .collect(),
                    &mut RequestContext {
                        eval_context: &self.eval_context,
                        poll_context: context,
                        host: &self.host,
                        transaction: branch.transaction.as_mut(),
                        activity: &mut activity,
                    },
                )?;
                if let Some(generation) = activity.observed_generation {
                    branch.observe(checkpoint.clone(), generation);
                }
                if activity.committed {
                    branch.retry = None;
                }
                match result {
                    RequestResult::Return(value) => MachineWork::Deliver {
                        value: value.into_core(),
                        branch,
                        scope_depth,
                    },
                    RequestResult::Alternatives(values) => {
                        let values = values
                            .into_iter()
                            .map(PublicValue::into_core)
                            .collect::<Vec<_>>();
                        match values.as_slice() {
                            [] => MachineWork::Outcome {
                                outcome: branch.into_failure(),
                                scope_depth,
                            },
                            [value] => MachineWork::Deliver {
                                value: value.clone(),
                                branch,
                                scope_depth,
                            },
                            _ => MachineWork::Drive {
                                branch: branch.with_effect(alternative_returns(&self.tags, values)),
                                scope_depth,
                            },
                        }
                    }
                    RequestResult::Scoped { operation, close } => {
                        branch
                            .control
                            .sequence
                            .push(Continuation::CloseScope(close.into_core()));
                        MachineWork::Drive {
                            branch: branch.with_effect(operation.into_core()),
                            scope_depth,
                        }
                    }
                    RequestResult::ReturnUnit => MachineWork::Deliver {
                        value: self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    },
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
        };
        drop(_phase_roots);
        Ok(MachineStep::Continue(work))
    }

    fn deliver_step(
        &mut self,
        context: &EvaluationPollContext,
        value: Value,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskHalt> {
        if let Some(continuation) = branch.control.sequence.last().cloned() {
            return match continuation {
                Continuation::Glam(function) => {
                    let function =
                        evaluate_owned(context, &self.eval_context, function)?.into_core();
                    branch.control.sequence.pop();
                    Ok(MachineStep::Continue(MachineWork::Apply {
                        function,
                        arguments: vec![value],
                        branch,
                        scope_depth,
                    }))
                }
                Continuation::RequireUnit => {
                    let value = evaluate_owned(context, &self.eval_context, value)?.into_core();
                    if value != self.eval_context.values().unit() {
                        return Err(TaskHalt::new(format!(
                            "effect task returned {}; expected unit",
                            value.diagnostic_kind_name()
                        )));
                    }
                    branch.control.sequence.pop();
                    Ok(MachineStep::Continue(MachineWork::Deliver {
                        value: self.eval_context.values().unit(),
                        branch,
                        scope_depth,
                    }))
                }
                Continuation::AssertUnit(diagnostic_context) => {
                    let assertion = Value::builtin_call(
                        self.eval_context.values(),
                        Builtin::AssertUnit,
                        vec![diagnostic_context, value, self.eval_context.values().unit()],
                    );
                    let value = evaluate_owned(context, &self.eval_context, assertion)?.into_core();
                    branch.control.sequence.pop();
                    Ok(MachineStep::Continue(MachineWork::Deliver {
                        value,
                        branch,
                        scope_depth,
                    }))
                }
                Continuation::Fix(handle) => {
                    let active = branch.active_fixes.last().ok_or_else(|| {
                        TaskHalt::new("reflection fixpoint lost its active branch")
                    })?;
                    if active.handle != handle {
                        return Err(TaskHalt::new(
                            "reflection fixpoint control became unbalanced",
                        ));
                    }
                    if active.next_choice != active.choices.len() {
                        return Err(TaskHalt::new("reflection fixpoint choice replay diverged"));
                    }
                    handle
                        .set(value.clone())
                        .map_err(|_| TaskHalt::new("reflection fixpoint initialized twice"))?;
                    branch.control.sequence.pop();
                    branch.active_fixes.pop();
                    Ok(MachineStep::Continue(MachineWork::Deliver {
                        value,
                        branch,
                        scope_depth,
                    }))
                }
                Continuation::CloseScope(close) => {
                    branch.control.sequence.pop();
                    branch
                        .control
                        .sequence
                        .push(Continuation::RestoreScopedValue(value));
                    branch.effect = close;
                    Ok(MachineStep::Continue(MachineWork::Drive {
                        branch,
                        scope_depth,
                    }))
                }
                Continuation::RestoreScopedValue(scoped_value) => {
                    let value = evaluate_owned(context, &self.eval_context, value)?.into_core();
                    if value != self.eval_context.values().unit() {
                        return Err(TaskHalt::new(format!(
                            "scoped effect close must return unit, got {value:?}"
                        )));
                    }
                    branch.control.sequence.pop();
                    Ok(MachineStep::Continue(MachineWork::Deliver {
                        value: scoped_value,
                        branch,
                        scope_depth,
                    }))
                }
            };
        }

        let mut resets = context.evaluate(&self.eval_context, |evaluator| {
            reset_frames_in(evaluator, &branch.state, &self.tags.continuation_state)
        })?;
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
            branch.state = replace_reset_frames(
                &self.eval_context,
                branch.state,
                &self.tags.continuation_state,
                &resets,
            );
            return Ok(MachineStep::Continue(MachineWork::Apply {
                function: frame.continuation,
                arguments: vec![value],
                branch,
                scope_depth,
            }));
        }
        let Some(_) = delimiter_order else {
            return Ok(MachineStep::Continue(MachineWork::Outcome {
                outcome: BranchOutcome::Complete(value, branch),
                scope_depth,
            }));
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
                let state = context
                    .evaluate(&self.eval_context, |evaluator| {
                        with_reset_stack_value_in(
                            evaluator,
                            branch.state.clone(),
                            &self.tags.continuation_state,
                            reset_stack,
                        )
                        .map(|state| evaluator.root_value(state))
                    })?
                    .into_core();
                branch.control.delimiters.pop();
                branch.state = state;
                branch.control = *outer;
            }
        }
        Ok(MachineStep::Continue(MachineWork::Deliver {
            value,
            branch,
            scope_depth,
        }))
    }

    fn enter_cut(
        &mut self,
        operation: Value,
        mut outer: Branch<S>,
        parent_scope_depth: usize,
    ) -> MachineWork<S> {
        let outer_sequence = std::mem::take(&mut outer.control.sequence);
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
        let mut initial = frame.outer.clone().with_effect(frame.operation.clone());
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
                Ok(MachineStep::Continue(MachineWork::Deliver {
                    value,
                    branch: completed,
                    scope_depth: frame.parent_scope_depth,
                }))
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
                    return Ok(MachineStep::Continue(restarted));
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
        frame.retry = Some(failed);
        let index = self.execution.cuts.len();
        self.execution.cuts.push(frame);
        Ok(MachineStep::Blocked(BlockedExecution::exhausted(
            RetryWake {
                observed_generation: generation,
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
                PublicValue::from_core(self.eval_context.values(), value),
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
                    return Ok(MachineStep::Continue(restarted));
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
                    PublicValue::from_core(self.eval_context.values(), value),
                    transaction,
                ));
                Ok(restarted
                    .map(MachineStep::Continue)
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
                    return Ok(MachineStep::Continue(restarted));
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
                    return Ok(MachineStep::Continue(restarted));
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
            MachineStep::Terminal(TaskTerminal::Complete(PublicValue::from_core(
                self.eval_context.values(),
                self.eval_context.values().unit(),
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

    fn waiting_block(&self, wait: EvaluationWaitToken) -> BlockedExecution<S> {
        BlockedExecution::waiting_on(wait, self.retry_wake())
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
                .work
                .branch()
                .and_then(|branch| branch.transaction.as_ref())
                .is_some_and(|transaction| transaction.observed);
            if frame_observed || branch_observed {
                let generation = self.execution.cuts[index]
                    .outer
                    .transaction
                    .as_ref()
                    .map(|transaction| transaction.snapshot.generation());
                if let Some(generation) = generation {
                    return Some(RetryWake {
                        observed_generation: generation,
                        action: WakeAction::RestartCut(index),
                    });
                }
            }
        }
        if self.search.retains_all()
            && let Some(transaction) = self
                .execution
                .work
                .branch()
                .and_then(|branch| branch.transaction.as_ref())
            && transaction.observed
        {
            return Some(RetryWake {
                observed_generation: transaction.snapshot.generation(),
                action: WakeAction::RestartSearch,
            });
        }
        let branch = self.execution.work.branch()?;
        let checkpoint = branch.retry.as_ref()?;
        let observed_generation = checkpoint.generation?;
        Some(RetryWake {
            observed_generation,
            action: WakeAction::ReplaceWork(Box::new(MachineWork::Drive {
                branch: (*checkpoint.branch).clone(),
                scope_depth: self.execution.work.scope_depth(),
            })),
        })
    }

    fn poll_blocked(&mut self) -> Option<EffectTaskPoll> {
        let blocked = self.blocked.as_ref()?;
        if let Some(retry) = &blocked.retry
            && self.host.snapshot().generation() != retry.observed_generation
        {
            let retry = self
                .blocked
                .take()
                .and_then(|blocked| blocked.retry)
                .expect("changed retry generation must retain its wake action");
            self.apply_wake(retry.action);
            return None;
        }
        let BlockReason::WaitingOn(wait) = &blocked.reason else {
            return Some(self.blocked_poll());
        };
        match self.eval_context.poll_wait(wait) {
            EvaluationWaitPoll::Pending(_) => {
                if matches!(
                    self.eval_context.pump_wait(wait, 256),
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
        let exit = self.exit.as_ref()?;
        let changed = exit
            .restart
            .as_ref()
            .is_some_and(|retry| self.host.snapshot().generation() != retry.observed_generation);
        if !changed {
            return Some(EffectTaskPoll::Exit(exit.poll.clone()));
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
            lazy: blocked.lazy(),
            observed_generation: blocked.observed_generation(),
            error: blocked.error(),
        })
    }

    fn apply_wake(&mut self, wake: WakeAction<S>) {
        match wake {
            WakeAction::ReplaceWork(work) => self.execution.work = *work,
            WakeAction::RestartCut(index) => self.restart_cut(index),
            WakeAction::RestartSearch => self.restart_search(),
        }
    }

    fn restart_search(&mut self) {
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
        self.blocked = None;
        self.exit = None;
        self.terminal = Some(terminal);
    }

    fn effect_request_in(
        &self,
        context: &EvaluatorStepContext<'_>,
        effect: Value,
    ) -> Result<Request<S::Request>, TaskHalt> {
        self.effect_request_values_in(context, effect)
            .map(|request| request.map_values(|value| self.root_request_value(context, value)))
    }

    fn effect_request_values_in(
        &self,
        context: &EvaluatorStepContext<'_>,
        effect: Value,
    ) -> Result<Request<S::Request, Value>, TaskHalt> {
        let effect = evaluate_in(context, effect)?;
        let Value::Dict(effect) = effect else {
            return Err(TaskHalt::new(format!(
                "reflection task requires an effect object, got {effect:?}"
            )));
        };
        let function = effect
            .get(&*keys::EFF)
            .cloned()
            .ok_or_else(|| TaskHalt::new("reflection effect has no `eff` member"))?;
        let function = evaluate_in(context, function)
            .map_err(|halt| halt.with_core_context(effect_dispatch_context("function")))?;
        let request = apply_in(context, function, vec![self.api.clone()])
            .map_err(|halt| halt.with_core_context(effect_dispatch_context("application")))?;
        let request = evaluate_in(context, request)
            .map_err(|halt| halt.with_core_context(effect_dispatch_context("request")))?;
        parse_request_values_in(context, request, &self.tags, &self.specialized_requests)
    }
}

fn effect_dispatch_context(stage: &str) -> Value {
    let stage_key = Key::binary_from_text("stage");
    let stage = Value::Atom(Atom::from_key(&Key::binary_from_text(stage)));
    eval::evaluation_context_frame_with_args(
        "effect_dispatch",
        Dict::new_sync().insert(stage_key, stage),
    )
}

pub(super) struct UnitEffectTask<S: TaskSpecialization>(pub(super) EffectTask<S>);

pub(super) struct ValueEffectTask<S: TaskSpecialization>(pub(super) EffectTask<S>);

pub(super) struct ContextualValueEffectTask<S: TaskSpecialization> {
    pub(super) task: EffectTask<S>,
    pub(super) context: Value,
}

impl<S: TaskSpecialization> EvaluationTaskMachine for ValueEffectTask<S> {
    fn poll(
        &mut self,
        context: &crate::evaluation::EvaluationPollContext,
        step_budget: usize,
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
        step_budget: usize,
    ) -> EvaluationMachinePoll {
        match poll_value_effect_task(&mut self.task, context, step_budget) {
            EvaluationMachinePoll::Failed(error) => {
                EvaluationMachinePoll::Failed(Arc::new(error.with_context(self.context.clone())))
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
    step_budget: usize,
) -> EvaluationMachinePoll {
    let observed_epoch = task.eval_context.current_observation_epoch();
    match task.poll_with_context(context, step_budget) {
        EffectTaskPoll::Yielded => EvaluationMachinePoll::Yielded,
        EffectTaskPoll::Blocked(blocked) => EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
            dependency: blocked.lazy.map(WorkDependency::Wait),
            observed_epoch: blocked.observed_generation.map(|_| observed_epoch),
            error: blocked.error,
        }),
        EffectTaskPoll::Exit(exit) => EvaluationMachinePoll::Exit(EvaluationExitBlock {
            intent: exit.intent,
            observed_epoch: exit.observed_generation.map(|_| observed_epoch),
        }),
        EffectTaskPoll::Complete(value) => {
            EvaluationMachinePoll::Complete(value.into_runtime_root())
        }
        EffectTaskPoll::Failed(error) => EvaluationMachinePoll::Failed(error.into_failure()),
        EffectTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
    }
}

impl<S: TaskSpecialization> EvaluationTaskMachine for UnitEffectTask<S> {
    fn poll(
        &mut self,
        context: &crate::evaluation::EvaluationPollContext,
        step_budget: usize,
    ) -> EvaluationMachinePoll {
        let observed_epoch = self.0.eval_context.current_observation_epoch();
        match self.0.poll_with_context(context, step_budget) {
            EffectTaskPoll::Yielded => EvaluationMachinePoll::Yielded,
            EffectTaskPoll::Blocked(blocked) => {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: blocked.lazy.map(WorkDependency::Wait),
                    observed_epoch: blocked.observed_generation.map(|_| observed_epoch),
                    error: blocked.error,
                })
            }
            EffectTaskPoll::Exit(exit) => EvaluationMachinePoll::Exit(EvaluationExitBlock {
                intent: exit.intent,
                observed_epoch: exit.observed_generation.map(|_| observed_epoch),
            }),
            EffectTaskPoll::Complete(value)
                if value.as_core() == &self.0.eval_context.values().unit() =>
            {
                EvaluationMachinePoll::Complete(value.into_runtime_root())
            }
            EffectTaskPoll::Complete(value) => {
                EvaluationMachinePoll::Failed(Arc::new(EvaluationFailure::message(format!(
                    "effect task returned {}; expected unit",
                    value.as_core().diagnostic_kind_name()
                ))))
            }
            EffectTaskPoll::Failed(error) => EvaluationMachinePoll::Failed(error.into_failure()),
            EffectTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
        }
    }

    fn cancel(&mut self) {
        self.0.finish(TaskTerminal::Cancelled);
    }
}

#[derive(Clone)]
struct Branch<S: TaskSpecialization> {
    effect: Value,
    control: Control,
    state: Value,
    transaction: Option<Transaction<S>>,
    active_fixes: Vec<ActiveFix<S>>,
    fix_restarts: Vec<FixRestart<S>>,
    retry: Option<RetryCheckpoint<S>>,
}

impl<S: TaskSpecialization> Branch<S> {
    fn new(effect: Value, state: Value) -> Self {
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

    fn with_effect(&self, effect: Value) -> Self {
        let mut branch = self.clone();
        branch.effect = effect;
        branch
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

#[derive(Clone)]
struct TaskExecution<S: TaskSpecialization> {
    work: MachineWork<S>,
    cuts: Vec<CutFrame<S>>,
}

#[derive(Clone)]
enum MachineWork<S: TaskSpecialization> {
    Drive {
        branch: Branch<S>,
        scope_depth: usize,
    },
    Deliver {
        value: Value,
        branch: Branch<S>,
        scope_depth: usize,
    },
    Apply {
        function: Value,
        arguments: Vec<Value>,
        branch: Branch<S>,
        scope_depth: usize,
    },
    Outcome {
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    },
}

impl<S: TaskSpecialization> MachineWork<S> {
    fn branch(&self) -> Option<&Branch<S>> {
        match self {
            Self::Drive { branch, .. }
            | Self::Deliver { branch, .. }
            | Self::Apply { branch, .. } => Some(branch),
            Self::Outcome { outcome, .. } => outcome.branch(),
        }
    }

    fn branch_mut(&mut self) -> Option<&mut Branch<S>> {
        match self {
            Self::Drive { branch, .. }
            | Self::Deliver { branch, .. }
            | Self::Apply { branch, .. } => Some(branch),
            Self::Outcome { outcome, .. } => outcome.branch_mut(),
        }
    }

    fn scope_depth(&self) -> usize {
        match self {
            Self::Drive { scope_depth, .. }
            | Self::Deliver { scope_depth, .. }
            | Self::Apply { scope_depth, .. }
            | Self::Outcome { scope_depth, .. } => *scope_depth,
        }
    }
}

#[derive(Clone)]
enum BranchOutcome<S: TaskSpecialization> {
    Complete(Value, Branch<S>),
    Fork(Box<Branch<S>>, Box<Branch<S>>),
    Fail(Branch<S>),
    Retry(Branch<S>),
    Cancelled,
}

impl<S: TaskSpecialization> BranchOutcome<S> {
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
    operation: Value,
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

const EFFECT_FUSION_BUDGET: usize = 32;

struct FusionState {
    initial_sequence_depth: usize,
    pending_sequence_values: Vec<Value>,
    effect_dirty: bool,
    state_dirty: bool,
}

impl FusionState {
    fn new(initial_sequence_depth: usize) -> Self {
        Self {
            initial_sequence_depth,
            pending_sequence_values: Vec::new(),
            effect_dirty: false,
            state_dirty: false,
        }
    }
}

enum FusedRequestAction<R> {
    Continue,
    Deliver(Value),
    Get(Value),
    Set(Value, Value),
    Boundary(Request<R, Value>),
}

enum PreparedDrive<R> {
    Request {
        request: Request<R>,
        _phase_roots: Vec<RuntimeValueRoot>,
    },
    Continue {
        _phase_roots: Vec<RuntimeValueRoot>,
    },
}

// This value is short-lived on the Rust stack. Boxing `Continue` would add an
// allocation to every cooperative machine transition merely to shrink the two
// uncommon terminal variants.
#[allow(clippy::large_enum_variant)]
enum MachineStep<S: TaskSpecialization> {
    Continue(MachineWork<S>),
    Blocked(BlockedExecution<S>),
    Exit(ExitIntent),
    Terminal(TaskTerminal),
}

struct BlockedExecution<S: TaskSpecialization> {
    reason: BlockReason,
    retry: Option<RetryWake<S>>,
}

impl<S: TaskSpecialization> BlockedExecution<S> {
    fn waiting_on(wait: EvaluationWaitToken, retry: Option<RetryWake<S>>) -> Self {
        Self {
            reason: BlockReason::WaitingOn(wait),
            retry,
        }
    }

    fn exhausted(retry: RetryWake<S>) -> Self {
        Self {
            reason: BlockReason::Exhausted,
            retry: Some(retry),
        }
    }

    fn evaluation_error(error: TaskHalt, retry: RetryWake<S>) -> Self {
        assert!(
            error.blocked_on().is_none(),
            "a blocked task error belongs in the wait dependency field"
        );
        Self {
            reason: BlockReason::EvaluationError(error),
            retry: Some(retry),
        }
    }

    fn lazy(&self) -> Option<EvaluationWaitToken> {
        match &self.reason {
            BlockReason::WaitingOn(wait) => Some(wait.clone()),
            BlockReason::Exhausted | BlockReason::EvaluationError(_) => None,
        }
    }

    fn observed_generation(&self) -> Option<u64> {
        self.retry.as_ref().map(|retry| retry.observed_generation)
    }

    fn error(&self) -> Option<Arc<EvaluationFailure>> {
        match &self.reason {
            BlockReason::EvaluationError(error) => Some(
                error
                    .permanent_failure()
                    .expect("evaluation-error blocks retain permanent failures")
                    .clone(),
            ),
            BlockReason::WaitingOn(_) | BlockReason::Exhausted => None,
        }
    }
}

enum BlockReason {
    WaitingOn(EvaluationWaitToken),
    Exhausted,
    EvaluationError(TaskHalt),
}

struct RetryWake<S: TaskSpecialization> {
    observed_generation: u64,
    action: WakeAction<S>,
}

enum WakeAction<S: TaskSpecialization> {
    ReplaceWork(Box<MachineWork<S>>),
    RestartCut(usize),
    RestartSearch,
}

pub(super) struct TaskBlock {
    pub(super) lazy: Option<EvaluationWaitToken>,
    pub(super) observed_generation: Option<u64>,
    pub(super) error: Option<Arc<EvaluationFailure>>,
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
    function: Value,
    entry: Branch<S>,
    scope_depth: usize,
}

#[derive(Clone)]
struct ActiveFix<S: TaskSpecialization> {
    root: Arc<FixRoot<S>>,
    choices: Vec<FixChoice>,
    next_choice: usize,
    handle: PromisedValue,
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
    Glam(Value),
    RequireUnit,
    AssertUnit(Value),
    Fix(PromisedValue),
    CloseScope(Value),
    RestoreScopedValue(Value),
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
        reset_stack: Value,
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
    continuation: Value,
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

impl<R, V> Request<R, V> {
    fn map_values<U>(self, mut map: impl FnMut(V) -> U) -> Request<R, U> {
        match self {
            Self::Return(value) => Request::Return(map(value)),
            Self::Seq(operation, continuation) => Request::Seq(map(operation), map(continuation)),
            Self::Alt(left, right) => Request::Alt(map(left), map(right)),
            Self::Fail => Request::Fail,
            Self::Cut(operation) => Request::Cut(map(operation)),
            Self::Fix(function) => Request::Fix(map(function)),
            Self::Get(path) => Request::Get(map(path)),
            Self::Set(path, value) => Request::Set(map(path), map(value)),
            Self::HeapGet(path) => Request::HeapGet(map(path)),
            Self::HeapSet(path, value) => Request::HeapSet(map(path), map(value)),
            Self::HeapRewrite(path, updater) => Request::HeapRewrite(map(path), map(updater)),
            Self::VolumeGet(volume, path) => Request::VolumeGet(volume, map(path)),
            Self::VolumeSet(volume, path, value) => {
                Request::VolumeSet(volume, map(path), map(value))
            }
            Self::VolumeRewrite(volume, path, updater) => {
                Request::VolumeRewrite(volume, map(path), map(updater))
            }
            Self::Reset(key, operation) => Request::Reset(map(key), map(operation)),
            Self::Shift(key, function) => Request::Shift(map(key), map(function)),
            Self::Resume(task, continuation, value) => {
                Request::Resume(task, continuation, map(value))
            }
            Self::ExitSuccess => Request::ExitSuccess,
            Self::ExitError(message) => Request::ExitError(map(message)),
            Self::Specialized(request, arguments) => {
                Request::Specialized(request, arguments.into_iter().map(map).collect())
            }
        }
    }
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

fn parse_request_values_in<R: Clone>(
    context: &EvaluatorStepContext<'_>,
    value: Value,
    tags: &Tags,
    specialized: &[SpecializedRequest<R>],
) -> Result<Request<R, Value>, TaskHalt> {
    let Value::Dict(dict) = value else {
        return Err(TaskHalt::new("effect API returned a non-request value"));
    };
    let parse = |tag: &Key| -> Result<Option<Vec<Value>>, TaskHalt> {
        dict.get(tag)
            .map(|payload| {
                let Value::List(payload) = evaluate_in(context, payload.clone())? else {
                    return Err(TaskHalt::new("effect request payload must be a list"));
                };
                eval::list_to_value_items_in(context, &payload).map_err(task_eval_error)
            })
            .transpose()
    };
    macro_rules! args {
        ($tag:expr, $n:literal, $body:expr) => {
            if let Some(arguments) = parse($tag)? {
                let arguments: [Value; $n] = arguments.try_into().map_err(|_| {
                    TaskHalt::new("effect request contained the wrong number of arguments")
                })?;
                return Ok(($body)(arguments));
            }
        };
    }
    args!(&tags.r, 1, |[value]: [Value; 1]| { Request::Return(value) });
    args!(&tags.seq, 2, |[operation, continuation]: [Value; 2]| {
        Request::Seq(operation, continuation)
    });
    args!(&tags.alt, 2, |[left, right]: [Value; 2]| {
        Request::Alt(left, right)
    });
    args!(&tags.fail, 0, |[]: [Value; 0]| { Request::Fail });
    args!(&tags.cut, 1, |[operation]: [Value; 1]| {
        Request::Cut(operation)
    });
    args!(&tags.fix, 1, |[function]: [Value; 1]| {
        Request::Fix(function)
    });
    args!(&tags.get, 1, |[path]: [Value; 1]| { Request::Get(path) });
    args!(&tags.set, 2, |[path, value]: [Value; 2]| {
        Request::Set(path, value)
    });
    args!(&tags.heap_get, 1, |[path]: [Value; 1]| {
        Request::HeapGet(path)
    });
    args!(&tags.heap_set, 2, |[path, value]: [Value; 2]| {
        Request::HeapSet(path, value)
    });
    args!(&tags.heap_rewrite, 2, |[path, updater]: [Value; 2]| {
        Request::HeapRewrite(path, updater)
    });
    args!(&tags.reset, 2, |[key, operation]: [Value; 2]| {
        Request::Reset(key, operation)
    });
    args!(&tags.shift, 2, |[key, function]: [Value; 2]| {
        Request::Shift(key, function)
    });
    args!(&tags.exit_success, 0, |[]: [Value; 0]| {
        Request::ExitSuccess
    });
    args!(&tags.exit_error, 1, |[message]: [Value; 1]| {
        Request::ExitError(message)
    });
    if let Some(arguments) = parse(&tags.resume)? {
        let [task_id, continuation_id, value]: [Value; 3] = arguments
            .try_into()
            .map_err(|_| TaskHalt::new("resume request contained the wrong number of arguments"))?;
        let task_id = request_id_in(context, task_id, "task")?;
        let task_id = EvaluationTaskId::from_u64(task_id)
            .ok_or_else(|| TaskHalt::new("reflection task ID must be nonzero"))?;
        return Ok(Request::Resume(
            task_id,
            request_id_in(context, continuation_id, "continuation")?,
            value,
        ));
    }
    for specialized in specialized {
        if let Some(arguments) = parse(&specialized.tag)? {
            if arguments.len() != specialized.arity {
                return Err(TaskHalt::new(
                    "effect request contained the wrong number of arguments",
                ));
            }
            return Ok(Request::Specialized(specialized.request.clone(), arguments));
        }
    }
    for (tag, _) in dict.iter() {
        let Some(identity) = parse_volume_request_tag(tag)? else {
            continue;
        };
        let arguments = parse(tag)?.expect("the request tag came from this dictionary");
        return match (identity.operation, arguments.as_slice()) {
            (VolumeOperation::Get, [path]) => Ok(Request::VolumeGet(identity.volume, path.clone())),
            (VolumeOperation::Set, [path, value]) => Ok(Request::VolumeSet(
                identity.volume,
                path.clone(),
                value.clone(),
            )),
            (VolumeOperation::Rewrite, [path, updater]) => Ok(Request::VolumeRewrite(
                identity.volume,
                path.clone(),
                updater.clone(),
            )),
            _ => Err(TaskHalt::new(
                "volume capability request contained the wrong number of arguments",
            )),
        };
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
    let entry = |name: &str, operation, arity| {
        (
            Key::atom_from_text(name),
            request_function(
                volume_request_tag(volume, operation),
                arity,
                Vec::new(),
                true,
            ),
        )
    };
    PublicValue::from_core(
        values,
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
        ),
    )
}

fn request_id_in(
    context: &EvaluatorStepContext<'_>,
    value: Value,
    kind: &str,
) -> Result<u64, TaskHalt> {
    let Value::Number(value) = evaluate_in(context, value)? else {
        return Err(TaskHalt::new(format!(
            "resume request has an invalid {kind} ID"
        )));
    };
    value
        .to_u64_if_integer()
        .ok_or_else(|| TaskHalt::new(format!("resume request has an invalid {kind} ID")))
}

fn effect_api<R: Clone>(
    tags: &Tags,
    specs: Vec<EffectRequestSpec<R>>,
    expose_shared_heap: bool,
    expose_exit: bool,
) -> Result<(Value, Vec<SpecializedRequest<R>>), TaskHalt> {
    let entry = |name: &str, value| (Key::atom_from_text(name), value);
    let heap_api = Value::Dict(
        [
            entry(
                "get",
                request_function(tags.heap_get.clone(), 1, Vec::new(), false),
            ),
            entry(
                "set",
                request_function(tags.heap_set.clone(), 2, Vec::new(), false),
            ),
            entry(
                "rewrite",
                request_function(tags.heap_rewrite.clone(), 2, Vec::new(), false),
            ),
        ]
        .into_iter()
        .fold(Dict::new_sync(), |dict, (key, value)| {
            dict.insert(key, value)
        }),
    );
    let mut entries = vec![
        entry("r", request_function(tags.r.clone(), 1, Vec::new(), false)),
        entry(
            "seq",
            request_function(tags.seq.clone(), 2, Vec::new(), false),
        ),
        entry(
            "alt",
            request_function(tags.alt.clone(), 2, Vec::new(), false),
        ),
        entry("fail", nullary_request(tags.fail.clone())),
        entry(
            "cut",
            request_function(tags.cut.clone(), 1, Vec::new(), false),
        ),
        entry(
            "fix",
            request_function(tags.fix.clone(), 1, Vec::new(), false),
        ),
        entry(
            "get",
            request_function(tags.get.clone(), 1, Vec::new(), false),
        ),
        entry(
            "set",
            request_function(tags.set.clone(), 2, Vec::new(), false),
        ),
        entry(
            "reset",
            request_function(tags.reset.clone(), 2, Vec::new(), false),
        ),
        entry(
            "shift",
            request_function(tags.shift.clone(), 2, Vec::new(), false),
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
                        request_function(tags.exit_error.clone(), 1, Vec::new(), false),
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
    let mut requests = Vec::with_capacity(specs.len());
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
                request_function(tag.clone(), spec.arity, Vec::new(), false)
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
    Ok((Value::Dict(api), requests))
}

fn insert_effect_api_path(
    api: Dict,
    path: &[Arc<str>],
    value: Value,
    display_path: &str,
) -> Result<Dict, TaskHalt> {
    let Some((name, rest)) = path.split_first() else {
        return Err(TaskHalt::new("effect API path must not be empty"));
    };
    let key = Key::atom_from_text(name);
    if rest.is_empty() {
        if api.get(&key).is_some() {
            return Err(TaskHalt::new(format!(
                "duplicate effect API name `{display_path}`"
            )));
        }
        return Ok(api.insert(key, value));
    }

    let nested = match api.get(&key) {
        Some(Value::Dict(nested)) => nested.clone(),
        Some(_) => {
            return Err(TaskHalt::new(format!(
                "effect API path `{display_path}` crosses non-dictionary `{name}`"
            )));
        }
        None => Dict::new_sync(),
    };
    let nested = insert_effect_api_path(nested, rest, value, display_path)?;
    Ok(api.insert(key, Value::Dict(nested)))
}

fn request_function(tag: Key, arity: usize, supplied: Vec<Value>, wrap_effect: bool) -> Value {
    let remaining = arity - supplied.len();
    let mut net = NetBuilder::<CoreSpecialization>::new();
    let exposed = net.unary_operator(eval::request_operator(
        tag,
        arity,
        Arc::from(supplied),
        wrap_effect,
    ));
    Value::Function(FunctionValue::new(
        NetValue::new(net.finish(exposed).instantiate_shared()),
        remaining,
    ))
}

fn nullary_request(tag: Key) -> Value {
    Value::Dict(Dict::new_sync().insert(tag, Value::List(List::empty())))
}

fn alternative_returns(tags: &Tags, values: Vec<Value>) -> Value {
    values
        .into_iter()
        .rev()
        .map(|value| eval::constant_effect(request_value(&tags.r, vec![value])))
        .reduce(|right, left| eval::constant_effect(request_value(&tags.alt, vec![left, right])))
        .expect("alternative return construction requires at least two values")
}

fn fuse_glam_delivery_in<S: TaskSpecialization>(
    context: &EvaluatorStepContext<'_>,
    branch: &mut Branch<S>,
    value: Value,
    initial_sequence_depth: usize,
    pending_sequence_values: &mut Vec<Value>,
) -> Result<Option<Value>, TaskHalt> {
    let Some(Continuation::Glam(function)) = branch.control.sequence.last().cloned() else {
        return Ok(None);
    };
    let function = evaluate_in(context, function)?;
    let removes_pending = branch.control.sequence.len() > initial_sequence_depth;
    branch.control.sequence.pop();
    if removes_pending {
        pending_sequence_values
            .pop()
            .expect("a fused sequence continuation must retain its pending value");
    }
    apply_in(context, function, vec![value]).map(Some)
}

fn apply_in(
    context: &EvaluatorStepContext<'_>,
    function: Value,
    arguments: Vec<Value>,
) -> Result<Value, TaskHalt> {
    eval::apply_values_in(context, function, arguments).map_err(task_eval_error)
}

fn evaluate_owned(
    poll: &EvaluationPollContext,
    context: &EvalContext,
    value: Value,
) -> Result<RuntimeValueRoot, TaskHalt> {
    poll.evaluate(context, |evaluator| {
        evaluate_in(evaluator, value).map(|value| evaluator.root_value(value))
    })
}

fn evaluate_in(context: &EvaluatorStepContext<'_>, value: Value) -> Result<Value, TaskHalt> {
    let mut value = value;
    while matches!(value, Value::Lazy(_) | Value::Promised(_)) {
        value = eval::eval_value_in(context, &value).map_err(task_eval_error)?;
    }
    Ok(value)
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

fn missing_volume_value(context: &EvalContext, volume: VolumeId) -> Value {
    Value::error(
        context.values(),
        format!("reflection volume {} has been revoked", volume.get()),
    )
}

fn value_key_in(context: &EvaluatorStepContext<'_>, value: Value) -> Result<Key, TaskHalt> {
    Key::from_value(&evaluate_in(context, value)?)
        .ok_or_else(|| TaskHalt::new("effect index is not keyable"))
}

fn get_value_path_in(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
    path: &[Key],
) -> Result<Value, TaskHalt> {
    let mut current = value.clone();
    for key in path {
        let Value::Dict(dict) = evaluate_in(context, current)? else {
            return Err(TaskHalt::new("state path traverses a non-dictionary value"));
        };
        current = dict
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
    }
    Ok(current)
}

fn lazy_value_path(context: &EvalContext, value: Value, path: &[Key]) -> Value {
    if path.is_empty() {
        return value;
    }
    Value::Lazy(LazyValue::from_access(
        context.values(),
        Arc::from(
            path.iter()
                .cloned()
                .map(CoreDataKey::Key)
                .collect::<Vec<_>>(),
        ),
        Arc::from([value]),
    ))
}

fn set_state_path_in(
    context: &EvaluatorStepContext<'_>,
    state: Value,
    path: &Value,
    value: Value,
) -> Result<Value, TaskHalt> {
    let path = eval::eval_key_path_list_in(context, path).map_err(task_eval_error)?;
    if path.is_empty() {
        return require_state_dict_in(context, value);
    }
    let path = Value::List(List::from_values(
        path.into_iter()
            .map(|key| key.to_value_with(context.context().values()))
            .collect(),
    ));
    evaluate_in(
        context,
        Value::builtin_call(
            context.context().values(),
            crate::core::Builtin::DictUpdate,
            vec![path, value, require_state_dict_in(context, state)?],
        ),
    )
}

fn require_state_dict_in(
    context: &EvaluatorStepContext<'_>,
    value: Value,
) -> Result<Value, TaskHalt> {
    match evaluate_in(context, value)? {
        value @ Value::Dict(_) => Ok(value),
        _ => Err(TaskHalt::new("reflection user state must be a dictionary")),
    }
}

fn reset_stack_value_in(
    context: &EvaluatorStepContext<'_>,
    state: &Value,
    continuation_state: &Key,
) -> Result<Value, TaskHalt> {
    let Value::Dict(state) = state else {
        return Err(TaskHalt::new("reflection user state must be a dictionary"));
    };
    let stack = state
        .get(continuation_state)
        .cloned()
        .unwrap_or_else(|| Value::List(List::empty()));
    reset_frames_from_value_in(context, &stack)?;
    Ok(stack)
}

fn reset_frames_in(
    context: &EvaluatorStepContext<'_>,
    state: &Value,
    continuation_state: &Key,
) -> Result<Vec<ResetFrame>, TaskHalt> {
    reset_frames_from_value_in(
        context,
        &reset_stack_value_in(context, state, continuation_state)?,
    )
}

fn reset_frames_from_value_in(
    context: &EvaluatorStepContext<'_>,
    stack: &Value,
) -> Result<Vec<ResetFrame>, TaskHalt> {
    let Value::List(stack) = evaluate_in(context, stack.clone())? else {
        return Err(TaskHalt::new(
            "reflection continuation state must be a list",
        ));
    };
    eval::list_to_value_items_in(context, &stack)
        .map_err(task_eval_error)?
        .into_iter()
        .map(|frame| {
            let Value::List(frame) = evaluate_in(context, frame)? else {
                return Err(TaskHalt::new(
                    "reflection continuation frame must be a list",
                ));
            };
            let [key, continuation, scope_depth, order]: [Value; 4] =
                eval::list_to_value_items_in(context, &frame)
                    .map_err(task_eval_error)?
                    .try_into()
                    .map_err(|_| {
                        TaskHalt::new("reflection continuation frame has the wrong size")
                    })?;
            let Value::Number(scope_depth) = scope_depth else {
                return Err(TaskHalt::new(
                    "reflection continuation frame has an invalid scope",
                ));
            };
            let Value::Number(order) = order else {
                return Err(TaskHalt::new(
                    "reflection continuation frame has an invalid order",
                ));
            };
            Ok(ResetFrame {
                key: value_key_in(context, key)?,
                continuation,
                scope_depth: scope_depth.to_usize_if_integer().ok_or_else(|| {
                    TaskHalt::new("reflection continuation frame has an invalid scope")
                })?,
                order: order.to_usize_if_integer().ok_or_else(|| {
                    TaskHalt::new("reflection continuation frame has an invalid order")
                })?,
            })
        })
        .collect()
}

fn reset_frames_value(values: &CoreValueFactory, frames: &[ResetFrame]) -> Value {
    Value::List(List::from_values(
        frames
            .iter()
            .map(|frame| {
                Value::List(List::from_values(vec![
                    frame.key.to_value_with(values),
                    frame.continuation.clone(),
                    Value::Number(Number::from_usize(frame.scope_depth)),
                    Value::Number(Number::from_usize(frame.order)),
                ]))
            })
            .collect(),
    ))
}

fn with_reset_frames_in(
    context: &EvaluatorStepContext<'_>,
    state: Value,
    continuation_state: &Key,
    frames: &[ResetFrame],
) -> Result<Value, TaskHalt> {
    with_reset_stack_value_in(
        context,
        state,
        continuation_state,
        reset_frames_value(context.context().values(), frames),
    )
}

fn replace_reset_frames(
    context: &EvalContext,
    state: Value,
    continuation_state: &Key,
    frames: &[ResetFrame],
) -> Value {
    let Value::Dict(state) = state else {
        return Value::error(
            context.values(),
            "reflection user state must remain a dictionary",
        );
    };
    Value::Dict(state.insert(
        continuation_state.clone(),
        reset_frames_value(context.values(), frames),
    ))
}

fn with_reset_stack_value_in(
    context: &EvaluatorStepContext<'_>,
    state: Value,
    continuation_state: &Key,
    stack: Value,
) -> Result<Value, TaskHalt> {
    reset_frames_from_value_in(context, &stack)?;
    let Value::Dict(state) = require_state_dict_in(context, state)? else {
        unreachable!("require_state_dict returned a non-dictionary")
    };
    Ok(Value::Dict(state.insert(continuation_state.clone(), stack)))
}

#[cfg(test)]
mod tests;
