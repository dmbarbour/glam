//! Resumable equality and ordering builtins.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluatedValue, EvaluationFailure, Key, List, RuntimeValueAccess,
    Value, keys,
};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, WorkDependency,
    poll_whnf_computation,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::application::effect_value;
use super::list_machine::{ListFrontMachine, ListFrontPoll};
use super::tagged_machine::{TaggedPayloadMachine, TaggedPayloadPoll};
use super::whnf::WhnfComputation;

pub(crate) enum ComparisonBuiltinPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

#[derive(Clone, Copy)]
enum ComparisonMode {
    Equality,
    Ordering,
}

#[derive(Clone, Copy)]
enum ComparisonResult {
    Equality(bool),
    Ordering(Ordering),
}

pub(crate) struct ComparisonBuiltinMachine {
    builtin: Builtin,
    frames: Vec<ComparisonFrame>,
    child_result: Option<ComparisonResult>,
}

#[allow(
    clippy::large_enum_variant,
    reason = "regional list-front state shrank the list variant before W6G.1f.3g converts this whole builtin family to one managed checkpoint"
)]
enum ComparisonFrame {
    Value(ValueComparisonFrame),
    List(ListComparisonFrame),
    Dict(DictEqualityFrame),
    Tuple(TupleOrderingFrame),
}

struct ValueComparisonFrame {
    mode: ComparisonMode,
    left: RuntimeValueRoot,
    right: RuntimeValueRoot,
    left_ready: Option<RuntimeValueRoot>,
    right_ready: Option<RuntimeValueRoot>,
    demand: Option<WhnfComputation>,
}

struct ListComparisonFrame {
    mode: ComparisonMode,
    left: ListFrontMachine,
    right: ListFrontMachine,
    left_item: Option<Option<(RuntimeValueRoot, RuntimeValueRoot)>>,
    right_item: Option<Option<(RuntimeValueRoot, RuntimeValueRoot)>>,
    awaiting_child: bool,
}

struct DictEqualityFrame {
    pairs: Vec<(RuntimeValueRoot, RuntimeValueRoot)>,
    next: usize,
    awaiting_child: bool,
}

enum TupleOrderingPhase {
    LeftTag,
    RightTag,
    LeftPayload,
    RightPayload,
    Comparing,
}

struct TupleOrderingFrame {
    left_tag: TaggedPayloadMachine,
    right_tag: TaggedPayloadMachine,
    left_payload: Option<RuntimeValueRoot>,
    right_payload: Option<RuntimeValueRoot>,
    left_list: Option<RuntimeValueRoot>,
    demand: Option<WhnfComputation>,
    phase: TupleOrderingPhase,
}

enum FrameAction {
    Replace(ComparisonFrame),
    Push(ComparisonFrame),
    Complete(ComparisonResult),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

enum TuplePayloadPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl ComparisonBuiltinMachine {
    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let [left, right]: [RuntimeValueRoot; 2] = arguments
            .try_into()
            .expect("a comparison source retains two operands");
        Self {
            builtin,
            frames: vec![ComparisonFrame::Value(ValueComparisonFrame::new(
                comparison_mode(builtin),
                left,
                right,
            ))],
            child_result: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ComparisonBuiltinPoll {
        if let Some(result) = self.child_result.take() {
            let Some(parent) = self.frames.last_mut() else {
                return self.finish(context, result);
            };
            if let Some(result) = parent.accept_child(result) {
                self.frames.pop();
                self.child_result = Some(result);
            }
            return ComparisonBuiltinPoll::Yielded;
        }

        let Some(frame) = self.frames.last_mut() else {
            unreachable!("comparison work must retain a frame or completed child")
        };
        let action = frame.poll(
            poll_context,
            context,
            durable_context,
            step_budget,
            comparison_name(self.builtin),
        );
        match action {
            FrameAction::Replace(frame) => {
                *self
                    .frames
                    .last_mut()
                    .expect("replacement requires the current frame") = frame;
                ComparisonBuiltinPoll::Yielded
            }
            FrameAction::Push(frame) => {
                self.frames.push(frame);
                ComparisonBuiltinPoll::Yielded
            }
            FrameAction::Complete(result) => {
                self.frames.pop();
                if self.frames.is_empty() {
                    self.finish(context, result)
                } else {
                    self.child_result = Some(result);
                    ComparisonBuiltinPoll::Yielded
                }
            }
            FrameAction::Pending(dependency) => ComparisonBuiltinPoll::Pending(dependency),
            FrameAction::Yielded => ComparisonBuiltinPoll::Yielded,
            FrameAction::Failed(failure) => ComparisonBuiltinPoll::Failed(failure),
        }
    }

    fn finish(
        &self,
        context: &EvaluatorStepContext<'_>,
        result: ComparisonResult,
    ) -> ComparisonBuiltinPoll {
        let success = match (self.builtin, result) {
            (Builtin::Greater, ComparisonResult::Ordering(ordering)) => {
                ordering == Ordering::Greater
            }
            (Builtin::GreaterEqual, ComparisonResult::Ordering(ordering)) => {
                ordering != Ordering::Less
            }
            (Builtin::LessEqual, ComparisonResult::Ordering(ordering)) => {
                ordering != Ordering::Greater
            }
            (Builtin::Less, ComparisonResult::Ordering(ordering)) => ordering == Ordering::Less,
            (Builtin::Equal, ComparisonResult::Equality(equal)) => equal,
            (Builtin::NotEqual, ComparisonResult::Equality(equal)) => !equal,
            _ => unreachable!("comparison result kind must match its builtin"),
        };
        let effect = if success { "r" } else { "fail" };
        let arguments = if success {
            vec![Value::Atom(crate::core::Atom::from_key(
                &Key::abstract_global_path(["builtin", "unit"]),
            ))]
        } else {
            Vec::new()
        };
        ComparisonBuiltinPoll::Ready(context.with_value_access(|access| {
            access.values().root_runtime_value(effect_value(
                access.values(),
                Value::PartialBuiltin(BuiltinCall {
                    builtin: Builtin::EffectCall,
                    arguments: Arc::from([
                        Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(effect))),
                        Value::List(List::from_values(arguments)),
                    ]),
                }),
            ))
        }))
    }
}

impl ComparisonFrame {
    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
        name: &'static str,
    ) -> FrameAction {
        match self {
            Self::Value(frame) => {
                frame.poll(poll_context, context, durable_context, step_budget, name)
            }
            Self::List(frame) => frame.poll(poll_context, context, durable_context, step_budget),
            Self::Dict(frame) => frame.poll(),
            Self::Tuple(frame) => {
                frame.poll(poll_context, context, durable_context, step_budget, name)
            }
        }
    }

    fn accept_child(&mut self, result: ComparisonResult) -> Option<ComparisonResult> {
        match self {
            Self::List(frame) => frame.accept_child(result),
            Self::Dict(frame) => frame.accept_child(result),
            Self::Tuple(frame) => frame.accept_child(result),
            Self::Value(_) => unreachable!("value frames are replaced by their recursive work"),
        }
    }
}

impl ValueComparisonFrame {
    fn new(mode: ComparisonMode, left: RuntimeValueRoot, right: RuntimeValueRoot) -> Self {
        Self {
            mode,
            left,
            right,
            left_ready: None,
            right_ready: None,
            demand: None,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
        name: &'static str,
    ) -> FrameAction {
        let source = if self.left_ready.is_none() {
            &self.left
        } else if self.right_ready.is_none() {
            &self.right
        } else {
            return classify_values(
                context,
                self.mode,
                &self.left_ready,
                &self.right_ready,
                name,
            );
        };
        let demand = self
            .demand
            .get_or_insert_with(|| WhnfComputation::from_root(source.clone()));
        let value = match poll_whnf_computation(demand, poll_context, durable_context, step_budget)
        {
            WhnfOwnerPoll::Ready(value) => value,
            WhnfOwnerPoll::Pending(dependency) => return FrameAction::Pending(dependency),
            WhnfOwnerPoll::Yielded => return FrameAction::Yielded,
            WhnfOwnerPoll::Failed(failure) => return FrameAction::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("comparison demand produced an external {boundary:?} boundary")
            }
        };
        self.demand = None;
        if self.left_ready.is_none() {
            self.left_ready = Some(value);
        } else {
            self.right_ready = Some(value);
        }
        FrameAction::Yielded
    }
}

fn classify_values(
    context: &EvaluatorStepContext<'_>,
    mode: ComparisonMode,
    left: &Option<RuntimeValueRoot>,
    right: &Option<RuntimeValueRoot>,
    name: &'static str,
) -> FrameAction {
    let left = left
        .as_ref()
        .expect("left comparison operand must be ready");
    let right = right
        .as_ref()
        .expect("right comparison operand must be ready");
    context.with_value_access(|access| {
        let left_value = EvaluatedValue::try_from(access.clone_root(left))
            .expect("comparison operand must be in WHNF");
        let right_value = EvaluatedValue::try_from(access.clone_root(right))
            .expect("comparison operand must be in WHNF");
        match mode {
            ComparisonMode::Ordering => {
                classify_ordering(access.values(), left, right, left_value, right_value, name)
            }
            ComparisonMode::Equality => {
                classify_equality(access.values(), left, right, left_value, right_value, name)
            }
        }
    })
}

fn classify_ordering(
    access: &RuntimeValueAccess<'_>,
    left_root: &RuntimeValueRoot,
    right_root: &RuntimeValueRoot,
    left: EvaluatedValue,
    right: EvaluatedValue,
    name: &'static str,
) -> FrameAction {
    match (left.into_value(), right.into_value()) {
        (Value::Lazy(_), _)
        | (_, Value::Lazy(_))
        | (Value::Promised(_), _)
        | (_, Value::Promised(_)) => unreachable!("comparison demand removes deferred values"),
        (Value::Number(left), Value::Number(right)) => {
            FrameAction::Complete(ComparisonResult::Ordering(left.cmp(&right)))
        }
        (Value::Binary(left), Value::Binary(right)) => {
            FrameAction::Complete(ComparisonResult::Ordering(left.cmp(&right)))
        }
        (Value::Binary(left), Value::List(_)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                ComparisonMode::Ordering,
                access.root_runtime_value(Value::List(List::from_bytes(left))),
                right_root.clone(),
            )))
        }
        (Value::List(_), Value::Binary(right)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                ComparisonMode::Ordering,
                left_root.clone(),
                access.root_runtime_value(Value::List(List::from_bytes(right))),
            )))
        }
        (Value::List(_), Value::List(_)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                ComparisonMode::Ordering,
                left_root.clone(),
                right_root.clone(),
            )))
        }
        (Value::Dict(left), Value::Dict(right)) => FrameAction::Replace(ComparisonFrame::Tuple(
            TupleOrderingFrame::new(access, &left, &right),
        )),
        (Value::Builtin(_), _)
        | (_, Value::Builtin(_))
        | (Value::PartialBuiltin(_), _)
        | (_, Value::PartialBuiltin(_))
        | (Value::Function(_), _)
        | (_, Value::Function(_))
        | (Value::Net(_), _)
        | (_, Value::Net(_)) => failure(context_message(
            access,
            format!("{name} builtin cannot compare function values"),
        )),
        (Value::Opaque(_), _) | (_, Value::Opaque(_)) => failure(context_message(
            access,
            format!("{name} builtin cannot compare opaque values"),
        )),
        (Value::Metadata(_), _) | (_, Value::Metadata(_)) => failure(context_message(
            access,
            format!("{name} builtin cannot compare sealed values"),
        )),
        (left, right) => failure(context_message(
            access,
            format!("{name} builtin cannot order values {left:?} and {right:?}"),
        )),
    }
}

fn classify_equality(
    access: &RuntimeValueAccess<'_>,
    left_root: &RuntimeValueRoot,
    right_root: &RuntimeValueRoot,
    left: EvaluatedValue,
    right: EvaluatedValue,
    name: &'static str,
) -> FrameAction {
    match (left.into_value(), right.into_value()) {
        (Value::Lazy(_), _)
        | (_, Value::Lazy(_))
        | (Value::Promised(_), _)
        | (_, Value::Promised(_)) => unreachable!("comparison demand removes deferred values"),
        (Value::Atom(left), Value::Atom(right)) => {
            FrameAction::Complete(ComparisonResult::Equality(left == right))
        }
        (Value::Number(left), Value::Number(right)) => {
            FrameAction::Complete(ComparisonResult::Equality(left == right))
        }
        (Value::Binary(left), Value::Binary(right)) => {
            FrameAction::Complete(ComparisonResult::Equality(left == right))
        }
        (Value::Binary(left), Value::List(_)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                ComparisonMode::Equality,
                access.root_runtime_value(Value::List(List::from_bytes(left))),
                right_root.clone(),
            )))
        }
        (Value::List(_), Value::Binary(right)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                ComparisonMode::Equality,
                left_root.clone(),
                access.root_runtime_value(Value::List(List::from_bytes(right))),
            )))
        }
        (Value::List(_), Value::List(_)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                ComparisonMode::Equality,
                left_root.clone(),
                right_root.clone(),
            )))
        }
        (Value::Dict(left), Value::Dict(right)) => FrameAction::Replace(ComparisonFrame::Dict(
            DictEqualityFrame::new(access, &left, &right),
        )),
        (Value::Builtin(_), _)
        | (_, Value::Builtin(_))
        | (Value::PartialBuiltin(_), _)
        | (_, Value::PartialBuiltin(_))
        | (Value::Function(_), _)
        | (_, Value::Function(_))
        | (Value::Net(_), _)
        | (_, Value::Net(_)) => failure(context_message(
            access,
            format!("{name} builtin cannot compare function values"),
        )),
        (Value::Opaque(left), Value::Opaque(right)) => {
            FrameAction::Complete(ComparisonResult::Equality(left == right))
        }
        (Value::Opaque(_), _) | (_, Value::Opaque(_)) => {
            FrameAction::Complete(ComparisonResult::Equality(false))
        }
        (Value::Metadata(_), _) | (_, Value::Metadata(_)) => failure(context_message(
            access,
            format!("{name} builtin cannot compare sealed values"),
        )),
        (Value::Atom(_), _)
        | (Value::Number(_), _)
        | (Value::Binary(_), _)
        | (Value::List(_), _)
        | (Value::Dict(_), _) => FrameAction::Complete(ComparisonResult::Equality(false)),
    }
}

impl ListComparisonFrame {
    fn new(mode: ComparisonMode, left: RuntimeValueRoot, right: RuntimeValueRoot) -> Self {
        Self {
            mode,
            left: ListFrontMachine::unowned(left),
            right: ListFrontMachine::unowned(right),
            left_item: None,
            right_item: None,
            awaiting_child: false,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> FrameAction {
        if self.awaiting_child {
            unreachable!("list comparison must receive its child before polling again")
        }
        if self.left_item.is_none() {
            self.left_item = Some(
                match self
                    .left
                    .poll(poll_context, context, durable_context, step_budget)
                {
                    ListFrontPoll::Ready(item) => item,
                    ListFrontPoll::Pending(dependency) => return FrameAction::Pending(dependency),
                    ListFrontPoll::Yielded => return FrameAction::Yielded,
                    ListFrontPoll::Failed(failure) => return FrameAction::Failed(failure),
                },
            );
            return FrameAction::Yielded;
        }
        if self.right_item.is_none() {
            self.right_item = Some(
                match self
                    .right
                    .poll(poll_context, context, durable_context, step_budget)
                {
                    ListFrontPoll::Ready(item) => item,
                    ListFrontPoll::Pending(dependency) => return FrameAction::Pending(dependency),
                    ListFrontPoll::Yielded => return FrameAction::Yielded,
                    ListFrontPoll::Failed(failure) => return FrameAction::Failed(failure),
                },
            );
            return FrameAction::Yielded;
        }

        let left = self
            .left_item
            .take()
            .expect("left list front must be ready");
        let right = self
            .right_item
            .take()
            .expect("right list front must be ready");
        match (left, right) {
            (None, None) => FrameAction::Complete(match self.mode {
                ComparisonMode::Equality => ComparisonResult::Equality(true),
                ComparisonMode::Ordering => ComparisonResult::Ordering(Ordering::Equal),
            }),
            (None, Some(_)) => FrameAction::Complete(match self.mode {
                ComparisonMode::Equality => ComparisonResult::Equality(false),
                ComparisonMode::Ordering => ComparisonResult::Ordering(Ordering::Less),
            }),
            (Some(_), None) => FrameAction::Complete(match self.mode {
                ComparisonMode::Equality => ComparisonResult::Equality(false),
                ComparisonMode::Ordering => ComparisonResult::Ordering(Ordering::Greater),
            }),
            (Some((left, left_tail)), Some((right, right_tail))) => {
                self.left = ListFrontMachine::unowned(left_tail);
                self.right = ListFrontMachine::unowned(right_tail);
                self.awaiting_child = true;
                FrameAction::Push(ComparisonFrame::Value(ValueComparisonFrame::new(
                    self.mode, left, right,
                )))
            }
        }
    }

    fn accept_child(&mut self, result: ComparisonResult) -> Option<ComparisonResult> {
        assert!(self.awaiting_child);
        self.awaiting_child = false;
        match result {
            ComparisonResult::Equality(true) | ComparisonResult::Ordering(Ordering::Equal) => None,
            result => Some(result),
        }
    }
}

impl DictEqualityFrame {
    fn new(access: &RuntimeValueAccess<'_>, left: &Dict, right: &Dict) -> Self {
        let empty = access.root_runtime_value(Value::Dict(Dict::new_sync()));
        let mut pairs = Vec::new();
        for (key, left_value) in left.iter() {
            pairs.push((
                access.root_runtime_value(access.duplicate_value(left_value)),
                right.get(key).map_or_else(
                    || empty.clone(),
                    |value| access.root_runtime_value(access.duplicate_value(value)),
                ),
            ));
        }
        for (key, right_value) in right.iter() {
            if !left.contains_key(key) {
                pairs.push((
                    empty.clone(),
                    access.root_runtime_value(access.duplicate_value(right_value)),
                ));
            }
        }
        Self {
            pairs,
            next: 0,
            awaiting_child: false,
        }
    }

    fn poll(&mut self) -> FrameAction {
        if self.awaiting_child {
            unreachable!("dictionary comparison must receive its child before polling again")
        }
        let Some((left, right)) = self.pairs.get(self.next) else {
            return FrameAction::Complete(ComparisonResult::Equality(true));
        };
        self.next += 1;
        self.awaiting_child = true;
        FrameAction::Push(ComparisonFrame::Value(ValueComparisonFrame::new(
            ComparisonMode::Equality,
            left.clone(),
            right.clone(),
        )))
    }

    fn accept_child(&mut self, result: ComparisonResult) -> Option<ComparisonResult> {
        assert!(self.awaiting_child);
        self.awaiting_child = false;
        match result {
            ComparisonResult::Equality(true) => None,
            ComparisonResult::Equality(false) => Some(result),
            ComparisonResult::Ordering(_) => {
                unreachable!("dictionary equality child must produce equality")
            }
        }
    }
}

impl TupleOrderingFrame {
    fn new(access: &RuntimeValueAccess<'_>, left: &Dict, right: &Dict) -> Self {
        Self {
            left_tag: TaggedPayloadMachine::new(access, left, &keys::TUPLE),
            right_tag: TaggedPayloadMachine::new(access, right, &keys::TUPLE),
            left_payload: None,
            right_payload: None,
            left_list: None,
            demand: None,
            phase: TupleOrderingPhase::LeftTag,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
        name: &'static str,
    ) -> FrameAction {
        match self.phase {
            TupleOrderingPhase::LeftTag => {
                match self
                    .left_tag
                    .poll(poll_context, context, durable_context, step_budget)
                {
                    TaggedPayloadPoll::Ready(Some(payload)) => {
                        self.left_payload = Some(payload);
                        self.phase = TupleOrderingPhase::RightTag;
                        FrameAction::Yielded
                    }
                    TaggedPayloadPoll::Ready(None) => failure_message(
                        context,
                        format!("{name} builtin can only order dictionaries tagged as `tuple`"),
                    ),
                    TaggedPayloadPoll::Pending(dependency) => FrameAction::Pending(dependency),
                    TaggedPayloadPoll::Yielded => FrameAction::Yielded,
                    TaggedPayloadPoll::Failed(failure) => FrameAction::Failed(failure),
                }
            }
            TupleOrderingPhase::RightTag => {
                match self
                    .right_tag
                    .poll(poll_context, context, durable_context, step_budget)
                {
                    TaggedPayloadPoll::Ready(Some(payload)) => {
                        self.right_payload = Some(payload);
                        self.phase = TupleOrderingPhase::LeftPayload;
                        FrameAction::Yielded
                    }
                    TaggedPayloadPoll::Ready(None) => failure_message(
                        context,
                        format!("{name} builtin can only order dictionaries tagged as `tuple`"),
                    ),
                    TaggedPayloadPoll::Pending(dependency) => FrameAction::Pending(dependency),
                    TaggedPayloadPoll::Yielded => FrameAction::Yielded,
                    TaggedPayloadPoll::Failed(failure) => FrameAction::Failed(failure),
                }
            }
            TupleOrderingPhase::LeftPayload => {
                match demand_tuple_payload(
                    &mut self.demand,
                    self.left_payload
                        .as_ref()
                        .expect("left tuple payload must be retained"),
                    poll_context,
                    context,
                    durable_context,
                    step_budget,
                    name,
                ) {
                    TuplePayloadPoll::Ready(list) => {
                        self.left_list = Some(list);
                        self.phase = TupleOrderingPhase::RightPayload;
                        FrameAction::Yielded
                    }
                    TuplePayloadPoll::Pending(dependency) => FrameAction::Pending(dependency),
                    TuplePayloadPoll::Yielded => FrameAction::Yielded,
                    TuplePayloadPoll::Failed(failure) => FrameAction::Failed(failure),
                }
            }
            TupleOrderingPhase::RightPayload => {
                match demand_tuple_payload(
                    &mut self.demand,
                    self.right_payload
                        .as_ref()
                        .expect("right tuple payload must be retained"),
                    poll_context,
                    context,
                    durable_context,
                    step_budget,
                    name,
                ) {
                    TuplePayloadPoll::Ready(list) => {
                        self.phase = TupleOrderingPhase::Comparing;
                        FrameAction::Push(ComparisonFrame::List(ListComparisonFrame::new(
                            ComparisonMode::Ordering,
                            self.left_list
                                .as_ref()
                                .expect("left tuple list must be ready")
                                .clone(),
                            list,
                        )))
                    }
                    TuplePayloadPoll::Pending(dependency) => FrameAction::Pending(dependency),
                    TuplePayloadPoll::Yielded => FrameAction::Yielded,
                    TuplePayloadPoll::Failed(failure) => FrameAction::Failed(failure),
                }
            }
            TupleOrderingPhase::Comparing => {
                unreachable!("tuple ordering must receive its child before polling again")
            }
        }
    }

    fn accept_child(&mut self, result: ComparisonResult) -> Option<ComparisonResult> {
        assert!(matches!(self.phase, TupleOrderingPhase::Comparing));
        Some(result)
    }
}

fn demand_tuple_payload(
    demand: &mut Option<WhnfComputation>,
    payload: &RuntimeValueRoot,
    poll_context: &EvaluationPollContext,
    context: &EvaluatorStepContext<'_>,
    durable_context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
    name: &'static str,
) -> TuplePayloadPoll {
    let computation = demand.get_or_insert_with(|| WhnfComputation::from_root(payload.clone()));
    let value = match poll_whnf_computation(computation, poll_context, durable_context, step_budget)
    {
        WhnfOwnerPoll::Ready(value) => value,
        WhnfOwnerPoll::Pending(dependency) => return TuplePayloadPoll::Pending(dependency),
        WhnfOwnerPoll::Yielded => return TuplePayloadPoll::Yielded,
        WhnfOwnerPoll::Failed(failure) => return TuplePayloadPoll::Failed(failure),
        WhnfOwnerPoll::External(boundary) => {
            unreachable!("tuple-payload demand produced an external {boundary:?} boundary")
        }
    };
    *demand = None;
    let list = context.with_value_access(|access| match access.clone_root(&value) {
        Value::Binary(bytes) => Ok(access
            .values()
            .root_runtime_value(Value::List(List::from_bytes(bytes)))),
        Value::List(_) => Ok(value),
        other => Err(format!(
            "{name} builtin requires tuple payloads to be lists or binaries, got {other:?}"
        )),
    });
    match list {
        Ok(list) => TuplePayloadPoll::Ready(list),
        Err(message) => match failure_message(context, message) {
            FrameAction::Failed(failure) => TuplePayloadPoll::Failed(failure),
            _ => unreachable!("failure construction must produce a failed frame action"),
        },
    }
}

fn comparison_mode(builtin: Builtin) -> ComparisonMode {
    match builtin {
        Builtin::Equal | Builtin::NotEqual => ComparisonMode::Equality,
        Builtin::Greater | Builtin::GreaterEqual | Builtin::LessEqual | Builtin::Less => {
            ComparisonMode::Ordering
        }
        _ => unreachable!("comparison machine received another builtin"),
    }
}

fn comparison_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Greater => "greater-than",
        Builtin::GreaterEqual => "greater-than-or-equal",
        Builtin::Equal => "equal",
        Builtin::NotEqual => "not-equal",
        Builtin::LessEqual => "less-than-or-equal",
        Builtin::Less => "less-than",
        _ => unreachable!("comparison name requested for another builtin"),
    }
}

fn context_message(access: &RuntimeValueAccess<'_>, message: String) -> RuntimeFailureRoot {
    access.root_runtime_failure(Arc::new(EvaluationFailure::message(message)))
}

fn failure(failure: RuntimeFailureRoot) -> FrameAction {
    FrameAction::Failed(failure)
}

fn failure_message(context: &EvaluatorStepContext<'_>, message: String) -> FrameAction {
    FrameAction::Failed(context.root_failure(Arc::new(EvaluationFailure::message(message))))
}
