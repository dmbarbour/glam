//! Resumable list and binary observation builtins.
//!
//! Indices are demanded and validated before the subject. Logical lists then
//! advance through the regional front/back projections, retaining the exact
//! remainder and the part taken so far beneath the containing managed builtin
//! checkpoint. `head` and `tail` take one item; the other observers take one
//! strict leaf at a time, all strict leaves in one step, and stop only to
//! force a deferred chunk. What `split`, `split_end` and `slice` take comes
//! back as a flat slice or a finger-tree rope of the leaves it spans.

use std::ops::ControlFlow;
use std::sync::Arc;

use bytes::Bytes;
use glam_gc::Visitor;

use crate::core::{
    Builtin, EvaluatedValue, EvaluationFailure, EvaluationHalt, LazyId, List, Value,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};
use crate::number::Number;

use super::builtin_machine::RegionalBuiltinPoll;
use super::list_machine::{
    RegionalListBack, RegionalListBackPoll, RegionalListFront, RegionalListFrontPoll, item_value_in,
};
use super::value::{evaluation_context_frame_in, index_from_evaluated, split_result_value};
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalListObservationMachine {
    operation: ListObservation,
    index_sources: Vec<Value>,
    index_demand: Option<RegionalWhnfWork>,
    indices: Vec<usize>,
    source: Option<Value>,
    source_demand: Option<RegionalWhnfWork>,
    walk: Option<RegionalListWalk>,
    source_owner: LazyId,
}

#[derive(Clone, Copy)]
enum ListObservation {
    Slice,
    Len,
    Split,
    SplitEnd,
    At,
    Head,
    Tail,
}

struct RegionalListWalk {
    operation: ListWalkOperation,
    front: Option<RegionalListFront>,
    back: Option<RegionalListBack>,
    /// Strict leaves taken so far, each a `Value::List`, in walk order.
    taken: Vec<Value>,
    /// Items passed so far, from the front, or from the back for `split_end`.
    position: usize,
    source_owner: LazyId,
}

#[derive(Clone, Copy)]
enum ListWalkOperation {
    Slice { start: usize, end: usize },
    Len,
    Split { index: usize },
    SplitEnd { count: usize },
    At { index: usize },
    Head,
    Tail,
}

impl RegionalListObservationMachine {
    pub(in crate::eval) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::Slice
                | Builtin::ListLen
                | Builtin::ListSplit
                | Builtin::ListSplitEnd
                | Builtin::ListAt
                | Builtin::ListHead
                | Builtin::ListTail
        )
    }

    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let operation = match builtin {
            Builtin::Slice => ListObservation::Slice,
            Builtin::ListLen => ListObservation::Len,
            Builtin::ListSplit => ListObservation::Split,
            Builtin::ListSplitEnd => ListObservation::SplitEnd,
            Builtin::ListAt => ListObservation::At,
            Builtin::ListHead => ListObservation::Head,
            Builtin::ListTail => ListObservation::Tail,
            _ => unreachable!("list observation machine received another builtin"),
        };
        let (source, indices) = arguments
            .split_last()
            .expect("a list observation retains its source operand");
        Self {
            operation,
            index_sources: indices
                .iter()
                .rev()
                .map(|value| access.values().duplicate_value(value))
                .collect(),
            index_demand: None,
            indices: Vec::with_capacity(indices.len()),
            source: Some(access.values().duplicate_value(source)),
            source_demand: None,
            walk: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if self.index_demand.is_none()
            && let Some(index) = self.index_sources.pop()
        {
            self.index_demand = Some(
                RegionalWhnfWork::from_focus(access, index).with_source_owner(self.source_owner),
            );
        }
        if let Some(index) = &mut self.index_demand {
            let value =
                match drive_regional_in_place(access, index, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(contextual_index_failure(
                            access,
                            failure,
                            self.index_context(),
                        ));
                    }
                };
            self.index_demand = None;
            let value = EvaluatedValue::from_whnf_in(access.values(), value)
                .expect("list observation index demand must reach WHNF");
            let index =
                match index_from_evaluated(access.values(), value, self.index_builtin_name()) {
                    Ok(index) => index,
                    Err(error) => {
                        return RegionalBuiltinPoll::Failed(error.into_permanent_failure());
                    }
                };
            self.indices.push(index);
            return RegionalBuiltinPoll::Yielded;
        }

        if matches!(self.operation, ListObservation::Slice) && self.indices[0] > self.indices[1] {
            return failure("slice builtin requires start to be less than or equal to end");
        }

        if self.walk.is_none() {
            if self.source_demand.is_none() {
                let source = self
                    .source
                    .take()
                    .expect("list observation must retain its undemanded source");
                self.source_demand = Some(
                    RegionalWhnfWork::from_focus(access, source)
                        .with_source_owner(self.source_owner),
                );
            }
            let source = match drive_regional_in_place(
                access,
                self.source_demand
                    .as_mut()
                    .expect("list observation source demand must be installed"),
                step_budget,
                reduce_semantic_shell,
            ) {
                RegionalWhnfStatus::Ready(value) => {
                    EvaluatedValue::from_whnf_in(access.values(), value)
                        .expect("list observation source demand must reach WHNF")
                        .into_value_in(access.values())
                }
                RegionalWhnfStatus::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalWhnfStatus::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };
            self.source_demand = None;

            match source {
                Value::Binary(bytes) => return self.finish_binary(access, bytes),
                source @ Value::List(_) => {
                    let operation = self.list_operation();
                    if let Some(result) = immediate_empty_prefix_result(access, operation, &source)
                    {
                        return RegionalBuiltinPoll::Ready(result);
                    }
                    self.walk = Some(RegionalListWalk::new_in(
                        access,
                        operation,
                        source,
                        self.source_owner,
                    ));
                    return RegionalBuiltinPoll::Yielded;
                }
                _ => return failure(self.subject_error()),
            }
        }

        self.walk
            .as_mut()
            .expect("list observation walk must be installed once")
            .poll_in(access, step_budget)
    }

    fn index_context(&self) -> &'static str {
        if matches!(self.operation, ListObservation::SplitEnd) {
            "list count"
        } else {
            "list_index"
        }
    }

    fn index_builtin_name(&self) -> &'static str {
        match self.operation {
            ListObservation::Slice => "slice",
            ListObservation::Split => "split",
            ListObservation::SplitEnd => "split_end",
            ListObservation::At => "list at",
            ListObservation::Len | ListObservation::Head | ListObservation::Tail => {
                unreachable!("this list observation has no index")
            }
        }
    }

    fn subject_error(&self) -> &'static str {
        match self.operation {
            ListObservation::Slice => "slice builtin requires a list or binary value",
            ListObservation::Len => "list len builtin requires a list or binary value",
            ListObservation::Split => "split builtin requires a list or binary value",
            ListObservation::SplitEnd => "split_end builtin requires a list or binary value",
            ListObservation::At => "list at builtin requires a list or binary value",
            ListObservation::Head => "list head builtin requires a list or binary value",
            ListObservation::Tail => "list tail builtin requires a list or binary value",
        }
    }

    fn list_operation(&self) -> ListWalkOperation {
        match self.operation {
            ListObservation::Slice => ListWalkOperation::Slice {
                start: self.indices[0],
                end: self.indices[1],
            },
            ListObservation::Len => ListWalkOperation::Len,
            ListObservation::Split => ListWalkOperation::Split {
                index: self.indices[0],
            },
            ListObservation::SplitEnd => ListWalkOperation::SplitEnd {
                count: self.indices[0],
            },
            ListObservation::At => ListWalkOperation::At {
                index: self.indices[0],
            },
            ListObservation::Head => ListWalkOperation::Head,
            ListObservation::Tail => ListWalkOperation::Tail,
        }
    }

    fn finish_binary(
        &self,
        access: &EvaluationValueAccess<'_>,
        bytes: Bytes,
    ) -> RegionalBuiltinPoll {
        let result = match self.operation {
            ListObservation::Slice => {
                let [start, end] = self.indices.as_slice() else {
                    unreachable!("slice retains two indices")
                };
                if *end > bytes.len() {
                    return failure("slice builtin end is out of bounds");
                }
                Value::Binary(bytes.slice(*start..*end))
            }
            ListObservation::Len => Value::Number(Number::from_usize(bytes.len())),
            ListObservation::Split => {
                let index = self.indices[0];
                if index > bytes.len() {
                    return failure("split builtin index is out of bounds");
                }
                return RegionalBuiltinPoll::Ready(binary_split(access, &bytes, index));
            }
            ListObservation::SplitEnd => {
                let count = self.indices[0];
                if count > bytes.len() {
                    return failure("split_end builtin count is out of bounds");
                }
                return RegionalBuiltinPoll::Ready(binary_split(
                    access,
                    &bytes,
                    bytes.len() - count,
                ));
            }
            ListObservation::At => {
                let Some(byte) = bytes.get(self.indices[0]) else {
                    return failure("list at builtin index is out of bounds");
                };
                Value::Number(Number::from_u8(*byte))
            }
            ListObservation::Head => {
                let Some(byte) = bytes.first() else {
                    return failure("list head builtin requires a non-empty list or binary");
                };
                Value::Number(Number::from_u8(*byte))
            }
            ListObservation::Tail => {
                if bytes.is_empty() {
                    return failure("list tail builtin requires a non-empty list or binary");
                }
                Value::Binary(bytes.slice(1..bytes.len()))
            }
        };
        RegionalBuiltinPoll::Ready(result)
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for value in &self.index_sources {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(demand) = &self.index_demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(source) = &self.source {
            trace_compatibility_value_managed_edges(source, visitor);
        }
        if let Some(demand) = &self.source_demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(walk) = &self.walk {
            walk.trace_managed_edges(visitor);
        }
    }
}

impl RegionalListWalk {
    fn new_in(
        access: &EvaluationValueAccess<'_>,
        operation: ListWalkOperation,
        source: Value,
        source_owner: LazyId,
    ) -> Self {
        let (front, back) = if matches!(operation, ListWalkOperation::SplitEnd { .. }) {
            (
                None,
                Some(RegionalListBack::new_in(access, source, Some(source_owner))),
            )
        } else {
            (
                Some(RegionalListFront::new_in(
                    access,
                    source,
                    Some(source_owner),
                )),
                None,
            )
        };
        Self {
            operation,
            front,
            back,
            taken: Vec::new(),
            position: 0,
            source_owner,
        }
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match self.operation {
            ListWalkOperation::Head | ListWalkOperation::Tail => {
                self.poll_front_item_in(access, step_budget)
            }
            ListWalkOperation::SplitEnd { .. } => self.poll_back_leaves_in(access, step_budget),
            ListWalkOperation::Slice { .. }
            | ListWalkOperation::Len
            | ListWalkOperation::Split { .. }
            | ListWalkOperation::At { .. } => self.poll_front_leaves_in(access, step_budget),
        }
    }

    fn poll_front_item_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match self
            .front
            .as_mut()
            .expect("front-list observation owns front-list work")
            .poll_in(access, step_budget)
        {
            RegionalListFrontPoll::Ready(Some((item, tail))) => match self.operation {
                ListWalkOperation::Head => RegionalBuiltinPoll::Ready(item),
                ListWalkOperation::Tail => RegionalBuiltinPoll::Ready(tail),
                _ => unreachable!("only head and tail take one item"),
            },
            RegionalListFrontPoll::Ready(None) => self.finish_empty(),
            RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
            RegionalListFrontPoll::Yielded => RegionalBuiltinPoll::Yielded,
            RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
        }
    }

    /// Takes strict leaves from the front until the observation completes
    /// or a deferred chunk must be forced.
    fn poll_front_leaves_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        loop {
            let (leaf, tail) = match self
                .front
                .as_mut()
                .expect("front-list observation owns front-list work")
                .poll_leaf_in(access, step_budget)
            {
                RegionalListFrontPoll::Ready(Some(step)) => step,
                RegionalListFrontPoll::Ready(None) => return self.finish_empty(),
                RegionalListFrontPoll::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalListFrontPoll::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };
            match self.consume_front_leaf(access, leaf, tail) {
                ControlFlow::Break(result) => return RegionalBuiltinPoll::Ready(result),
                ControlFlow::Continue(tail) => {
                    self.front = Some(RegionalListFront::new_in(
                        access,
                        tail,
                        Some(self.source_owner),
                    ));
                }
            }
        }
    }

    /// Observes one leaf covering the items from `self.position`. Breaks
    /// with the result, or continues with the tail after the leaf.
    fn consume_front_leaf(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        leaf: List,
        tail: Value,
    ) -> ControlFlow<Value, Value> {
        let start = self.position;
        let len = leaf.known_len().expect("a strict leaf knows its length");
        let end = start + len;
        match self.operation {
            ListWalkOperation::At { index } if index < end => {
                let item = leaf.leaf_item_at_by(index - start, &mut |value| {
                    access.values().duplicate_value(value)
                });
                return ControlFlow::Break(item_value_in(access, item));
            }
            ListWalkOperation::Split { index } if index <= end => {
                let (taken, rest) = leaf.split_leaf_at(index - start);
                self.taken.push(Value::List(taken));
                let Value::List(tail) = tail else {
                    unreachable!("a front walk retains a list tail")
                };
                return ControlFlow::Break(split_result_value(
                    access.values(),
                    Value::List(self.join_taken(access)),
                    Value::List(List::concat(rest, tail)),
                ));
            }
            ListWalkOperation::Split { .. } => self.taken.push(Value::List(leaf)),
            ListWalkOperation::Slice {
                start: from,
                end: to,
            } => {
                // The part of this leaf inside the slice, as leaf offsets.
                let low = from.saturating_sub(start).min(len);
                let high = (to - start).min(len);
                if low < high {
                    let (init, _) = leaf.split_leaf_at(high);
                    let (_, piece) = init.split_leaf_at(low);
                    self.taken.push(Value::List(piece));
                }
                if to <= end {
                    return ControlFlow::Break(Value::List(self.join_taken(access)));
                }
            }
            ListWalkOperation::At { .. } | ListWalkOperation::Len => {}
            ListWalkOperation::Head
            | ListWalkOperation::Tail
            | ListWalkOperation::SplitEnd { .. } => {
                unreachable!("only position observers walk front leaves")
            }
        }
        self.position = end;
        ControlFlow::Continue(tail)
    }

    /// Takes strict leaves from the back for `split_end`, the mirror of
    /// [`Self::poll_front_leaves_in`].
    fn poll_back_leaves_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        let ListWalkOperation::SplitEnd { count } = self.operation else {
            unreachable!("only split-end walks from the back")
        };
        loop {
            let (init, leaf) = match self
                .back
                .as_mut()
                .expect("split-end traversal owns back-list work")
                .poll_leaf_in(access, step_budget)
            {
                RegionalListBackPoll::Ready(Some(step)) => step,
                RegionalListBackPoll::Ready(None) => {
                    return failure("split_end builtin count is out of bounds");
                }
                RegionalListBackPoll::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalListBackPoll::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalListBackPoll::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };
            let len = leaf.known_len().expect("a strict leaf knows its length");
            let wanted = count - self.position;
            if wanted <= len {
                let (rest, taken) = leaf.split_leaf_at(len - wanted);
                self.taken.push(Value::List(taken));
                self.taken.reverse();
                let Value::List(init) = init else {
                    unreachable!("a back walk retains a list init")
                };
                return RegionalBuiltinPoll::Ready(split_result_value(
                    access.values(),
                    Value::List(List::concat(init, rest)),
                    Value::List(self.join_taken(access)),
                ));
            }
            self.taken.push(Value::List(leaf));
            self.position += len;
            self.back = Some(RegionalListBack::new_in(
                access,
                init,
                Some(self.source_owner),
            ));
        }
    }

    /// Joins the leaves taken so far, in list order, as a flat slice or a
    /// finger-tree rope.
    fn join_taken(&mut self, access: &EvaluationValueAccess<'_>) -> List {
        let pieces = std::mem::take(&mut self.taken)
            .into_iter()
            .map(|piece| match piece {
                Value::List(piece) => piece,
                _ => unreachable!("taken pieces are lists"),
            })
            .collect();
        List::join_strict_pieces(pieces, &mut |value| access.values().duplicate_value(value))
    }

    fn finish_empty(&self) -> RegionalBuiltinPoll {
        match self.operation {
            ListWalkOperation::Len => {
                RegionalBuiltinPoll::Ready(Value::Number(Number::from_usize(self.position)))
            }
            ListWalkOperation::SplitEnd { .. } => {
                unreachable!("split-end empty results are handled by back traversal")
            }
            ListWalkOperation::At { .. } => failure("list at builtin index is out of bounds"),
            ListWalkOperation::Head => {
                failure("list head builtin requires a non-empty list or binary")
            }
            ListWalkOperation::Tail => {
                failure("list tail builtin requires a non-empty list or binary")
            }
            ListWalkOperation::Split { .. } => failure("split builtin index is out of bounds"),
            ListWalkOperation::Slice { .. } => failure("slice builtin end is out of bounds"),
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(front) = &self.front {
            front.trace_managed_edges(visitor);
        }
        if let Some(back) = &self.back {
            back.trace_managed_edges(visitor);
        }
        for piece in &self.taken {
            trace_compatibility_value_managed_edges(piece, visitor);
        }
    }
}

fn immediate_empty_prefix_result(
    access: &EvaluationValueAccess<'_>,
    operation: ListWalkOperation,
    source: &Value,
) -> Option<Value> {
    match operation {
        ListWalkOperation::Split { index: 0 } => Some(split_result_value(
            access.values(),
            Value::List(List::empty()),
            access.values().duplicate_value(source),
        )),
        ListWalkOperation::SplitEnd { count: 0 } => Some(split_result_value(
            access.values(),
            access.values().duplicate_value(source),
            Value::List(List::empty()),
        )),
        ListWalkOperation::Slice { end: 0, .. } => Some(Value::List(List::empty())),
        _ => None,
    }
}

fn binary_split(access: &EvaluationValueAccess<'_>, bytes: &Bytes, index: usize) -> Value {
    split_result_value(
        access.values(),
        Value::Binary(bytes.slice(0..index)),
        Value::Binary(bytes.slice(index..bytes.len())),
    )
}

fn contextual_index_failure(
    access: &EvaluationValueAccess<'_>,
    failure: Arc<EvaluationFailure>,
    operation: &str,
) -> Arc<EvaluationFailure> {
    EvaluationHalt::failure(failure)
        .with_context(
            access.values(),
            evaluation_context_frame_in(access.values(), operation),
        )
        .into_permanent_failure()
}

fn failure(message: &str) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message)))
}
