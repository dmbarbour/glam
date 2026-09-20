//! Resumable equality and ordering builtins.

use std::cmp::Ordering;
use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluatedValue, EvaluationFailure, Key, LazyId, List, Value, keys,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};

use super::application::effect_value;
use super::builtin_machine::RegionalBuiltinPoll;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::tagged_machine::{RegionalTaggedPayload, RegionalTaggedPayloadPoll};
use super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place,
    reduce_semantic_shell,
};

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

pub(in crate::eval) struct RegionalComparisonMachine {
    builtin: Builtin,
    frames: Vec<ComparisonFrame>,
    child_result: Option<ComparisonResult>,
}

enum ComparisonFrame {
    Value(ValueComparisonFrame),
    List(ListComparisonFrame),
    Dict(DictEqualityFrame),
    Tuple(TupleOrderingFrame),
}

struct ValueComparisonFrame {
    mode: ComparisonMode,
    left: Value,
    right: Value,
    left_ready: Option<Value>,
    right_ready: Option<Value>,
    demand: Option<RegionalWhnfWork>,
    source_owner: LazyId,
}

struct ListComparisonFrame {
    mode: ComparisonMode,
    left: RegionalListFront,
    right: RegionalListFront,
    left_item: Option<Option<(Value, Value)>>,
    right_item: Option<Option<(Value, Value)>>,
    awaiting_child: bool,
    source_owner: LazyId,
}

struct DictEqualityFrame {
    pairs: Vec<(Value, Value)>,
    next: usize,
    awaiting_child: bool,
    source_owner: LazyId,
}

enum TupleOrderingPhase {
    LeftTag,
    RightTag,
    LeftPayload,
    RightPayload,
    Comparing,
}

struct TupleOrderingFrame {
    left_tag: RegionalTaggedPayload,
    right_tag: RegionalTaggedPayload,
    left_payload: Option<Value>,
    right_payload: Option<Value>,
    left_list: Option<Value>,
    demand: Option<RegionalWhnfWork>,
    phase: TupleOrderingPhase,
    source_owner: LazyId,
}

enum FrameAction {
    Replace(ComparisonFrame),
    Push(ComparisonFrame),
    Complete(ComparisonResult),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

enum TuplePayloadPoll {
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

impl RegionalComparisonMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let [left, right] = arguments else {
            unreachable!("a comparison source retains two operands")
        };
        Self {
            builtin,
            frames: vec![ComparisonFrame::Value(ValueComparisonFrame::new_in(
                access,
                comparison_mode(builtin),
                left,
                right,
                source_owner,
            ))],
            child_result: None,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if let Some(result) = self.child_result.take() {
            let Some(parent) = self.frames.last_mut() else {
                return self.finish(access, result);
            };
            if let Some(result) = parent.accept_child(result) {
                self.frames.pop();
                self.child_result = Some(result);
            }
            return RegionalBuiltinPoll::Yielded;
        }

        let Some(frame) = self.frames.last_mut() else {
            unreachable!("comparison work must retain a frame or completed child")
        };
        let action = frame.poll(access, step_budget, comparison_name(self.builtin));
        match action {
            FrameAction::Replace(frame) => {
                *self
                    .frames
                    .last_mut()
                    .expect("replacement requires the current frame") = frame;
                RegionalBuiltinPoll::Yielded
            }
            FrameAction::Push(frame) => {
                self.frames.push(frame);
                RegionalBuiltinPoll::Yielded
            }
            FrameAction::Complete(result) => {
                self.frames.pop();
                if self.frames.is_empty() {
                    self.finish(access, result)
                } else {
                    self.child_result = Some(result);
                    RegionalBuiltinPoll::Yielded
                }
            }
            FrameAction::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
            FrameAction::Yielded => RegionalBuiltinPoll::Yielded,
            FrameAction::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
        }
    }

    fn finish(
        &self,
        access: &EvaluationValueAccess<'_>,
        result: ComparisonResult,
    ) -> RegionalBuiltinPoll {
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
        RegionalBuiltinPoll::Ready(effect_value(
            access.values(),
            Value::PartialBuiltin(BuiltinCall {
                builtin: Builtin::EffectCall,
                arguments: Arc::from([
                    Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(effect))),
                    Value::List(List::from_values(arguments)),
                ]),
            }),
        ))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for frame in &self.frames {
            frame.trace_managed_edges(visitor);
        }
    }
}

impl ComparisonFrame {
    fn poll(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
        name: &'static str,
    ) -> FrameAction {
        match self {
            Self::Value(frame) => frame.poll_in(access, step_budget, name),
            Self::List(frame) => frame.poll_in(access, step_budget),
            Self::Dict(frame) => frame.poll(access),
            Self::Tuple(frame) => frame.poll_in(access, step_budget, name),
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

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match self {
            Self::Value(frame) => frame.trace_managed_edges(visitor),
            Self::List(frame) => frame.trace_managed_edges(visitor),
            Self::Dict(frame) => frame.trace_managed_edges(visitor),
            Self::Tuple(frame) => frame.trace_managed_edges(visitor),
        }
    }
}

impl ValueComparisonFrame {
    fn new_in(
        access: &EvaluationValueAccess<'_>,
        mode: ComparisonMode,
        left: &Value,
        right: &Value,
        source_owner: LazyId,
    ) -> Self {
        Self {
            mode,
            left: access.values().duplicate_value(left),
            right: access.values().duplicate_value(right),
            left_ready: None,
            right_ready: None,
            demand: None,
            source_owner,
        }
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
        name: &'static str,
    ) -> FrameAction {
        let source = if self.left_ready.is_none() {
            &self.left
        } else if self.right_ready.is_none() {
            &self.right
        } else {
            return classify_values(
                access,
                self.mode,
                &self.left_ready,
                &self.right_ready,
                self.source_owner,
                name,
            );
        };
        let demand = self.demand.get_or_insert_with(|| {
            RegionalWhnfWork::from_focus(access, access.values().duplicate_value(source))
                .with_source_owner(self.source_owner)
        });
        let value =
            match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                RegionalWhnfStatus::Ready(value) => EvaluatedValue::try_from(value)
                    .expect("comparison operand must reach WHNF")
                    .into_value(),
                RegionalWhnfStatus::Boundary(request) => {
                    return FrameAction::Boundary(request);
                }
                RegionalWhnfStatus::Yielded => return FrameAction::Yielded,
                RegionalWhnfStatus::Failed(failure) => return FrameAction::Failed(failure),
            };
        self.demand = None;
        if self.left_ready.is_none() {
            self.left_ready = Some(value);
        } else {
            self.right_ready = Some(value);
        }
        FrameAction::Yielded
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.left, visitor);
        trace_compatibility_value_managed_edges(&self.right, visitor);
        if let Some(value) = &self.left_ready {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(value) = &self.right_ready {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

fn classify_values(
    access: &EvaluationValueAccess<'_>,
    mode: ComparisonMode,
    left: &Option<Value>,
    right: &Option<Value>,
    source_owner: LazyId,
    name: &'static str,
) -> FrameAction {
    let left = left
        .as_ref()
        .expect("left comparison operand must be ready");
    let right = right
        .as_ref()
        .expect("right comparison operand must be ready");
    let left_value = EvaluatedValue::try_from(access.values().duplicate_value(left))
        .expect("comparison operand must be in WHNF");
    let right_value = EvaluatedValue::try_from(access.values().duplicate_value(right))
        .expect("comparison operand must be in WHNF");
    match mode {
        ComparisonMode::Ordering => classify_ordering(
            access,
            left,
            right,
            left_value,
            right_value,
            source_owner,
            name,
        ),
        ComparisonMode::Equality => classify_equality(
            access,
            left,
            right,
            left_value,
            right_value,
            source_owner,
            name,
        ),
    }
}

fn classify_ordering(
    access: &EvaluationValueAccess<'_>,
    left_root: &Value,
    right_root: &Value,
    left: EvaluatedValue,
    right: EvaluatedValue,
    source_owner: LazyId,
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
                access,
                ComparisonMode::Ordering,
                Value::List(List::from_bytes(left)),
                access.values().duplicate_value(right_root),
                Some(source_owner),
            )))
        }
        (Value::List(_), Value::Binary(right)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                access,
                ComparisonMode::Ordering,
                access.values().duplicate_value(left_root),
                Value::List(List::from_bytes(right)),
                Some(source_owner),
            )))
        }
        (Value::List(_), Value::List(_)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                access,
                ComparisonMode::Ordering,
                access.values().duplicate_value(left_root),
                access.values().duplicate_value(right_root),
                Some(source_owner),
            )))
        }
        (Value::Dict(left), Value::Dict(right)) => FrameAction::Replace(ComparisonFrame::Tuple(
            TupleOrderingFrame::new_in(access, &left, &right, source_owner),
        )),
        (Value::Builtin(_), _)
        | (_, Value::Builtin(_))
        | (Value::PartialBuiltin(_), _)
        | (_, Value::PartialBuiltin(_))
        | (Value::Function(_), _)
        | (_, Value::Function(_))
        | (Value::Net(_), _)
        | (_, Value::Net(_)) => {
            failure_message(format!("{name} builtin cannot compare function values"))
        }
        (Value::Opaque(_), _) | (_, Value::Opaque(_)) => {
            failure_message(format!("{name} builtin cannot compare opaque values"))
        }
        (Value::Metadata(_), _) | (_, Value::Metadata(_)) => {
            failure_message(format!("{name} builtin cannot compare sealed values"))
        }
        (left, right) => failure_message(format!(
            "{name} builtin cannot order values {left:?} and {right:?}"
        )),
    }
}

fn classify_equality(
    access: &EvaluationValueAccess<'_>,
    left_root: &Value,
    right_root: &Value,
    left: EvaluatedValue,
    right: EvaluatedValue,
    source_owner: LazyId,
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
                access,
                ComparisonMode::Equality,
                Value::List(List::from_bytes(left)),
                access.values().duplicate_value(right_root),
                Some(source_owner),
            )))
        }
        (Value::List(_), Value::Binary(right)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                access,
                ComparisonMode::Equality,
                access.values().duplicate_value(left_root),
                Value::List(List::from_bytes(right)),
                Some(source_owner),
            )))
        }
        (Value::List(_), Value::List(_)) => {
            FrameAction::Replace(ComparisonFrame::List(ListComparisonFrame::new(
                access,
                ComparisonMode::Equality,
                access.values().duplicate_value(left_root),
                access.values().duplicate_value(right_root),
                Some(source_owner),
            )))
        }
        (Value::Dict(left), Value::Dict(right)) => FrameAction::Replace(ComparisonFrame::Dict(
            DictEqualityFrame::new_in(access, &left, &right, source_owner),
        )),
        (Value::Builtin(_), _)
        | (_, Value::Builtin(_))
        | (Value::PartialBuiltin(_), _)
        | (_, Value::PartialBuiltin(_))
        | (Value::Function(_), _)
        | (_, Value::Function(_))
        | (Value::Net(_), _)
        | (_, Value::Net(_)) => {
            failure_message(format!("{name} builtin cannot compare function values"))
        }
        (Value::Opaque(left), Value::Opaque(right)) => {
            FrameAction::Complete(ComparisonResult::Equality(left == right))
        }
        (Value::Opaque(_), _) | (_, Value::Opaque(_)) => {
            FrameAction::Complete(ComparisonResult::Equality(false))
        }
        (Value::Metadata(_), _) | (_, Value::Metadata(_)) => {
            failure_message(format!("{name} builtin cannot compare sealed values"))
        }
        (Value::Atom(_), _)
        | (Value::Number(_), _)
        | (Value::Binary(_), _)
        | (Value::List(_), _)
        | (Value::Dict(_), _) => FrameAction::Complete(ComparisonResult::Equality(false)),
    }
}

impl ListComparisonFrame {
    fn new(
        access: &EvaluationValueAccess<'_>,
        mode: ComparisonMode,
        left: Value,
        right: Value,
        source_owner: Option<LazyId>,
    ) -> Self {
        let source_owner = source_owner.expect("comparison list work must retain its source owner");
        Self {
            mode,
            left: RegionalListFront::new_in(access, left, Some(source_owner)),
            right: RegionalListFront::new_in(access, right, Some(source_owner)),
            left_item: None,
            right_item: None,
            awaiting_child: false,
            source_owner,
        }
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> FrameAction {
        if self.awaiting_child {
            unreachable!("list comparison must receive its child before polling again")
        }
        if self.left_item.is_none() {
            self.left_item = Some(match self.left.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(item) => item,
                RegionalListFrontPoll::Boundary(request) => {
                    return FrameAction::Boundary(request);
                }
                RegionalListFrontPoll::Yielded => return FrameAction::Yielded,
                RegionalListFrontPoll::Failed(failure) => {
                    return FrameAction::Failed(failure);
                }
            });
            return FrameAction::Yielded;
        }
        if self.right_item.is_none() {
            self.right_item = Some(match self.right.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(item) => item,
                RegionalListFrontPoll::Boundary(request) => {
                    return FrameAction::Boundary(request);
                }
                RegionalListFrontPoll::Yielded => return FrameAction::Yielded,
                RegionalListFrontPoll::Failed(failure) => {
                    return FrameAction::Failed(failure);
                }
            });
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
                self.left = RegionalListFront::new_in(access, left_tail, Some(self.source_owner));
                self.right = RegionalListFront::new_in(access, right_tail, Some(self.source_owner));
                self.awaiting_child = true;
                FrameAction::Push(ComparisonFrame::Value(ValueComparisonFrame::new_in(
                    access,
                    self.mode,
                    &left,
                    &right,
                    self.source_owner,
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

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.left.trace_managed_edges(visitor);
        self.right.trace_managed_edges(visitor);
        for item in [&self.left_item, &self.right_item] {
            if let Some(Some((value, tail))) = item {
                trace_compatibility_value_managed_edges(value, visitor);
                trace_compatibility_value_managed_edges(tail, visitor);
            }
        }
    }
}

impl DictEqualityFrame {
    fn new_in(
        access: &EvaluationValueAccess<'_>,
        left: &Dict,
        right: &Dict,
        source_owner: LazyId,
    ) -> Self {
        let empty = Value::Dict(Dict::new_sync());
        let mut pairs = Vec::new();
        for (key, left_value) in left.iter() {
            pairs.push((
                access.values().duplicate_value(left_value),
                right.get(key).map_or_else(
                    || access.values().duplicate_value(&empty),
                    |value| access.values().duplicate_value(value),
                ),
            ));
        }
        for (key, right_value) in right.iter() {
            if !left.contains_key(key) {
                pairs.push((
                    access.values().duplicate_value(&empty),
                    access.values().duplicate_value(right_value),
                ));
            }
        }
        Self {
            pairs,
            next: 0,
            awaiting_child: false,
            source_owner,
        }
    }

    fn poll(&mut self, access: &EvaluationValueAccess<'_>) -> FrameAction {
        if self.awaiting_child {
            unreachable!("dictionary comparison must receive its child before polling again")
        }
        let Some((left, right)) = self.pairs.get(self.next) else {
            return FrameAction::Complete(ComparisonResult::Equality(true));
        };
        self.next += 1;
        self.awaiting_child = true;
        FrameAction::Push(ComparisonFrame::Value(ValueComparisonFrame::new_in(
            access,
            ComparisonMode::Equality,
            left,
            right,
            self.source_owner,
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

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for (left, right) in &self.pairs {
            trace_compatibility_value_managed_edges(left, visitor);
            trace_compatibility_value_managed_edges(right, visitor);
        }
    }
}

impl TupleOrderingFrame {
    fn new_in(
        access: &EvaluationValueAccess<'_>,
        left: &Dict,
        right: &Dict,
        source_owner: LazyId,
    ) -> Self {
        Self {
            left_tag: RegionalTaggedPayload::new_in(access, left, &keys::TUPLE, source_owner),
            right_tag: RegionalTaggedPayload::new_in(access, right, &keys::TUPLE, source_owner),
            left_payload: None,
            right_payload: None,
            left_list: None,
            demand: None,
            phase: TupleOrderingPhase::LeftTag,
            source_owner,
        }
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
        name: &'static str,
    ) -> FrameAction {
        match self.phase {
            TupleOrderingPhase::LeftTag => match self.left_tag.poll_in(access, step_budget) {
                RegionalTaggedPayloadPoll::Ready(Some(payload)) => {
                    self.left_payload = Some(payload);
                    self.phase = TupleOrderingPhase::RightTag;
                    FrameAction::Yielded
                }
                RegionalTaggedPayloadPoll::Ready(None) => failure_message(format!(
                    "{name} builtin can only order dictionaries tagged as `tuple`"
                )),
                RegionalTaggedPayloadPoll::Boundary(request) => FrameAction::Boundary(request),
                RegionalTaggedPayloadPoll::Yielded => FrameAction::Yielded,
                RegionalTaggedPayloadPoll::Failed(failure) => FrameAction::Failed(failure),
            },
            TupleOrderingPhase::RightTag => match self.right_tag.poll_in(access, step_budget) {
                RegionalTaggedPayloadPoll::Ready(Some(payload)) => {
                    self.right_payload = Some(payload);
                    self.phase = TupleOrderingPhase::LeftPayload;
                    FrameAction::Yielded
                }
                RegionalTaggedPayloadPoll::Ready(None) => failure_message(format!(
                    "{name} builtin can only order dictionaries tagged as `tuple`"
                )),
                RegionalTaggedPayloadPoll::Boundary(request) => FrameAction::Boundary(request),
                RegionalTaggedPayloadPoll::Yielded => FrameAction::Yielded,
                RegionalTaggedPayloadPoll::Failed(failure) => FrameAction::Failed(failure),
            },
            TupleOrderingPhase::LeftPayload => {
                match demand_tuple_payload(
                    &mut self.demand,
                    self.left_payload
                        .as_ref()
                        .expect("left tuple payload must be retained"),
                    self.source_owner,
                    access,
                    step_budget,
                    name,
                ) {
                    TuplePayloadPoll::Ready(list) => {
                        self.left_list = Some(list);
                        self.phase = TupleOrderingPhase::RightPayload;
                        FrameAction::Yielded
                    }
                    TuplePayloadPoll::Boundary(request) => FrameAction::Boundary(request),
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
                    self.source_owner,
                    access,
                    step_budget,
                    name,
                ) {
                    TuplePayloadPoll::Ready(list) => {
                        self.phase = TupleOrderingPhase::Comparing;
                        FrameAction::Push(ComparisonFrame::List(ListComparisonFrame::new(
                            access,
                            ComparisonMode::Ordering,
                            access.values().duplicate_value(
                                self.left_list
                                    .as_ref()
                                    .expect("left tuple list must be ready"),
                            ),
                            list,
                            Some(self.source_owner),
                        )))
                    }
                    TuplePayloadPoll::Boundary(request) => FrameAction::Boundary(request),
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

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.left_tag.trace_managed_edges(visitor);
        self.right_tag.trace_managed_edges(visitor);
        for value in [&self.left_payload, &self.right_payload, &self.left_list]
            .into_iter()
            .flatten()
        {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

fn demand_tuple_payload(
    demand: &mut Option<RegionalWhnfWork>,
    payload: &Value,
    source_owner: LazyId,
    access: &EvaluationValueAccess<'_>,
    step_budget: &mut EvaluationStepBudget,
    name: &'static str,
) -> TuplePayloadPoll {
    let computation = demand.get_or_insert_with(|| {
        RegionalWhnfWork::from_focus(access, access.values().duplicate_value(payload))
            .with_source_owner(source_owner)
    });
    let value =
        match drive_regional_in_place(access, computation, step_budget, reduce_semantic_shell) {
            RegionalWhnfStatus::Ready(value) => EvaluatedValue::try_from(value)
                .expect("tuple payload must reach WHNF")
                .into_value(),
            RegionalWhnfStatus::Boundary(request) => {
                return TuplePayloadPoll::Boundary(request);
            }
            RegionalWhnfStatus::Yielded => return TuplePayloadPoll::Yielded,
            RegionalWhnfStatus::Failed(failure) => return TuplePayloadPoll::Failed(failure),
        };
    *demand = None;
    match value {
        Value::Binary(bytes) => TuplePayloadPoll::Ready(Value::List(List::from_bytes(bytes))),
        Value::List(_) => TuplePayloadPoll::Ready(value),
        other => TuplePayloadPoll::Failed(Arc::new(EvaluationFailure::message(format!(
            "{name} builtin requires tuple payloads to be lists or binaries, got {other:?}"
        )))),
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

fn failure_message(message: String) -> FrameAction {
    FrameAction::Failed(Arc::new(EvaluationFailure::message(message)))
}
