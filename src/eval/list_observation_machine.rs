//! Resumable list and binary observation builtins.
//!
//! Indices are demanded and validated before the subject. Logical lists then
//! advance through the regional front/back projections one item at a time,
//! retaining the exact suffix and completed prefix beneath the containing
//! managed builtin checkpoint.

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
    RegionalListBack, RegionalListBackPoll, RegionalListFront, RegionalListFrontPoll,
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
    items: Vec<Value>,
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
            let value = EvaluatedValue::try_from(value)
                .expect("list observation index demand must reach WHNF");
            let index = match index_from_evaluated(value, self.index_builtin_name()) {
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
                RegionalWhnfStatus::Ready(value) => EvaluatedValue::try_from(value)
                    .expect("list observation source demand must reach WHNF")
                    .into_value(),
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
            items: Vec::new(),
            position: 0,
            source_owner,
        }
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if matches!(self.operation, ListWalkOperation::SplitEnd { .. }) {
            return match self
                .back
                .as_mut()
                .expect("split-end traversal owns back-list work")
                .poll_in(access, step_budget)
            {
                RegionalListBackPoll::Ready(Some((init, item))) => {
                    self.consume_back_item(access, init, item)
                }
                RegionalListBackPoll::Ready(None) => {
                    failure("split_end builtin count is out of bounds")
                }
                RegionalListBackPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalListBackPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalListBackPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }
        match self
            .front
            .as_mut()
            .expect("front-list observation owns front-list work")
            .poll_in(access, step_budget)
        {
            RegionalListFrontPoll::Ready(Some((item, tail))) => {
                self.consume_item(access, item, tail)
            }
            RegionalListFrontPoll::Ready(None) => self.finish_empty(),
            RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
            RegionalListFrontPoll::Yielded => RegionalBuiltinPoll::Yielded,
            RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
        }
    }

    fn consume_item(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        item: Value,
        tail: Value,
    ) -> RegionalBuiltinPoll {
        match self.operation {
            ListWalkOperation::Head => RegionalBuiltinPoll::Ready(item),
            ListWalkOperation::Tail => RegionalBuiltinPoll::Ready(tail),
            ListWalkOperation::At { index } if self.position == index => {
                RegionalBuiltinPoll::Ready(item)
            }
            ListWalkOperation::At { .. } | ListWalkOperation::Len => {
                self.position += 1;
                self.front = Some(RegionalListFront::new_in(
                    access,
                    tail,
                    Some(self.source_owner),
                ));
                RegionalBuiltinPoll::Yielded
            }
            ListWalkOperation::Split { index } => {
                self.items.push(item);
                self.position += 1;
                if self.position == index {
                    RegionalBuiltinPoll::Ready(split_result_value(
                        access.values(),
                        Value::List(List::from_values(std::mem::take(&mut self.items))),
                        tail,
                    ))
                } else {
                    self.front = Some(RegionalListFront::new_in(
                        access,
                        tail,
                        Some(self.source_owner),
                    ));
                    RegionalBuiltinPoll::Yielded
                }
            }
            ListWalkOperation::Slice { start, end } => {
                if self.position >= start {
                    self.items.push(item);
                }
                self.position += 1;
                if self.position == end {
                    RegionalBuiltinPoll::Ready(Value::List(List::from_values(std::mem::take(
                        &mut self.items,
                    ))))
                } else {
                    self.front = Some(RegionalListFront::new_in(
                        access,
                        tail,
                        Some(self.source_owner),
                    ));
                    RegionalBuiltinPoll::Yielded
                }
            }
            ListWalkOperation::SplitEnd { .. } => unreachable!("split-end walks from the back"),
        }
    }

    fn consume_back_item(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        init: Value,
        item: Value,
    ) -> RegionalBuiltinPoll {
        let ListWalkOperation::SplitEnd { count } = self.operation else {
            unreachable!("only split-end walks from the back")
        };
        self.items.push(item);
        self.position += 1;
        if self.position == count {
            self.items.reverse();
            RegionalBuiltinPoll::Ready(split_result_value(
                access.values(),
                init,
                Value::List(List::from_values(std::mem::take(&mut self.items))),
            ))
        } else {
            self.back = Some(RegionalListBack::new_in(
                access,
                init,
                Some(self.source_owner),
            ));
            RegionalBuiltinPoll::Yielded
        }
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
        for value in &self.items {
            trace_compatibility_value_managed_edges(value, visitor);
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
