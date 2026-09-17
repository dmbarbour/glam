//! Resumable list and binary observation builtins.
//!
//! Indices are demanded and validated before the subject. Logical lists then
//! advance through [`ListFrontMachine`] one item at a time, retaining the
//! exact tail and any completed prefix across yields or dependencies.

use std::sync::Arc;

use bytes::Bytes;

use crate::core::{Builtin, EvaluatedValue, EvaluationFailure, EvaluationHalt, List, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::builtin_machine::BuiltinTaskPoll;
use super::list_machine::{ListBackMachine, ListBackPoll, ListFrontMachine, ListFrontPoll};
use super::value::{evaluation_context_frame_in, index_from_evaluated, split_result_value};
use super::whnf::WhnfComputation;

pub(crate) struct ListObservationMachine {
    operation: ListObservation,
    index_demands: Vec<WhnfComputation>,
    next_index: usize,
    indices: Vec<usize>,
    source: WhnfComputation,
    source_root: RuntimeValueRoot,
    walk: Option<ListWalk>,
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

struct ListWalk {
    operation: ListWalkOperation,
    front: Option<ListFrontMachine>,
    back: Option<ListBackMachine>,
    items: Vec<RuntimeValueRoot>,
    position: usize,
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

enum SourceShape {
    Binary(Bytes),
    List,
}

impl ListObservationMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
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

    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
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
        let mut arguments = arguments;
        let source_root = arguments
            .pop()
            .expect("a list observation retains its source operand");
        Self {
            operation,
            index_demands: arguments
                .into_iter()
                .map(WhnfComputation::from_root)
                .collect(),
            next_index: 0,
            indices: Vec::new(),
            source: WhnfComputation::from_root(source_root.clone()),
            source_root,
            walk: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if let Some(index) = self.index_demands.get_mut(self.next_index) {
            let value = match poll_demand(index, poll_context, durable_context, step_budget) {
                DemandPoll::Ready(value) => value,
                DemandPoll::Failed(failure) => {
                    return BuiltinTaskPoll::Failed(contextual_index_failure(
                        context,
                        failure,
                        self.index_context(),
                    ));
                }
                other => return other.into_builtin_poll(),
            };
            let value = context.with_value_access(|access| access.clone_root(&value));
            let value = EvaluatedValue::try_from(value)
                .expect("list observation index demand must reach WHNF");
            let index = match index_from_evaluated(value, self.index_builtin_name()) {
                Ok(index) => index,
                Err(error) => {
                    return BuiltinTaskPoll::Failed(
                        context.root_failure(error.into_permanent_failure()),
                    );
                }
            };
            self.indices.push(index);
            self.next_index += 1;
            return BuiltinTaskPoll::Yielded;
        }

        if matches!(self.operation, ListObservation::Slice) && self.indices[0] > self.indices[1] {
            return failure(
                context,
                "slice builtin requires start to be less than or equal to end",
            );
        }

        if self.walk.is_none() {
            let source =
                match poll_demand(&mut self.source, poll_context, durable_context, step_budget) {
                    DemandPoll::Ready(value) => value,
                    other => return other.into_builtin_poll(),
                };
            self.source_root = source.clone();
            let shape = context.with_value_access(|access| match access.clone_root(&source) {
                Value::Binary(bytes) => Ok(SourceShape::Binary(bytes)),
                Value::List(_) => Ok(SourceShape::List),
                _ => Err(self.subject_error()),
            });
            let shape = match shape {
                Ok(shape) => shape,
                Err(message) => return failure(context, message),
            };
            match shape {
                SourceShape::Binary(bytes) => return self.finish_binary(context, bytes),
                SourceShape::List => {
                    let operation = self.list_operation();
                    if let Some(result) =
                        immediate_empty_prefix_result(context, &operation, &self.source_root)
                    {
                        return result;
                    }
                    self.walk = Some(ListWalk {
                        operation,
                        front: (!matches!(operation, ListWalkOperation::SplitEnd { .. }))
                            .then(|| ListFrontMachine::unowned(self.source_root.clone())),
                        back: matches!(operation, ListWalkOperation::SplitEnd { .. })
                            .then(|| ListBackMachine::new(self.source_root.clone())),
                        items: Vec::new(),
                        position: 0,
                    });
                    return BuiltinTaskPoll::Yielded;
                }
            }
        }

        self.walk
            .as_mut()
            .expect("list observation walk must be installed once")
            .poll(poll_context, context, durable_context, step_budget)
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

    fn finish_binary(&self, context: &EvaluatorStepContext<'_>, bytes: Bytes) -> BuiltinTaskPoll {
        let result = match self.operation {
            ListObservation::Slice => {
                let [start, end] = self.indices.as_slice() else {
                    unreachable!("slice retains two indices")
                };
                if *end > bytes.len() {
                    return failure(context, "slice builtin end is out of bounds");
                }
                Value::Binary(bytes.slice(*start..*end))
            }
            ListObservation::Len => Value::Number(Number::from_usize(bytes.len())),
            ListObservation::Split => {
                let index = self.indices[0];
                if index > bytes.len() {
                    return failure(context, "split builtin index is out of bounds");
                }
                return rooted_binary_split(context, &bytes, index);
            }
            ListObservation::SplitEnd => {
                let count = self.indices[0];
                if count > bytes.len() {
                    return failure(context, "split_end builtin count is out of bounds");
                }
                let index = bytes.len() - count;
                return rooted_binary_split(context, &bytes, index);
            }
            ListObservation::At => {
                let Some(byte) = bytes.get(self.indices[0]) else {
                    return failure(context, "list at builtin index is out of bounds");
                };
                Value::Number(Number::from_u8(*byte))
            }
            ListObservation::Head => {
                let Some(byte) = bytes.first() else {
                    return failure(
                        context,
                        "list head builtin requires a non-empty list or binary",
                    );
                };
                Value::Number(Number::from_u8(*byte))
            }
            ListObservation::Tail => {
                if bytes.is_empty() {
                    return failure(
                        context,
                        "list tail builtin requires a non-empty list or binary",
                    );
                }
                Value::Binary(bytes.slice(1..bytes.len()))
            }
        };
        BuiltinTaskPoll::Ready(context.root_value(result))
    }
}

impl ListWalk {
    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if matches!(self.operation, ListWalkOperation::SplitEnd { .. }) {
            return match self
                .back
                .as_mut()
                .expect("split-end traversal owns back-list work")
                .poll(poll_context, context, durable_context, step_budget)
            {
                ListBackPoll::Ready(Some((init, item))) => {
                    self.consume_back_item(context, init, item)
                }
                ListBackPoll::Ready(None) => {
                    failure(context, "split_end builtin count is out of bounds")
                }
                ListBackPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ListBackPoll::Yielded => BuiltinTaskPoll::Yielded,
                ListBackPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            };
        }
        match self
            .front
            .as_mut()
            .expect("front-list observation owns front-list work")
            .poll(poll_context, context, durable_context, step_budget)
        {
            ListFrontPoll::Ready(Some((item, tail))) => self.consume_item(context, item, tail),
            ListFrontPoll::Ready(None) => self.finish_empty(context),
            ListFrontPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
            ListFrontPoll::Yielded => BuiltinTaskPoll::Yielded,
            ListFrontPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
        }
    }

    fn consume_item(
        &mut self,
        context: &EvaluatorStepContext<'_>,
        item: RuntimeValueRoot,
        tail: RuntimeValueRoot,
    ) -> BuiltinTaskPoll {
        match self.operation {
            ListWalkOperation::Head => BuiltinTaskPoll::Ready(item),
            ListWalkOperation::Tail => BuiltinTaskPoll::Ready(tail),
            ListWalkOperation::At { index } if self.position == index => {
                BuiltinTaskPoll::Ready(item)
            }
            ListWalkOperation::At { .. } | ListWalkOperation::Len => {
                self.position += 1;
                self.front = Some(ListFrontMachine::unowned(tail));
                BuiltinTaskPoll::Yielded
            }
            ListWalkOperation::Split { index } => {
                self.items.push(item);
                self.position += 1;
                if self.position == index {
                    rooted_split_from_items_and_root(context, &self.items, &tail)
                } else {
                    self.front = Some(ListFrontMachine::unowned(tail));
                    BuiltinTaskPoll::Yielded
                }
            }
            ListWalkOperation::Slice { start, end } => {
                if self.position >= start {
                    self.items.push(item);
                }
                self.position += 1;
                if self.position == end {
                    rooted_list_from_items(context, &self.items)
                } else {
                    self.front = Some(ListFrontMachine::unowned(tail));
                    BuiltinTaskPoll::Yielded
                }
            }
            ListWalkOperation::SplitEnd { .. } => unreachable!("split-end walks from the back"),
        }
    }

    fn consume_back_item(
        &mut self,
        context: &EvaluatorStepContext<'_>,
        init: RuntimeValueRoot,
        item: RuntimeValueRoot,
    ) -> BuiltinTaskPoll {
        let ListWalkOperation::SplitEnd { count } = self.operation else {
            unreachable!("only split-end walks from the back")
        };
        self.items.push(item);
        self.position += 1;
        if self.position == count {
            self.items.reverse();
            rooted_split_from_root_and_items(context, &init, &self.items)
        } else {
            self.back = Some(ListBackMachine::new(init));
            BuiltinTaskPoll::Yielded
        }
    }

    fn finish_empty(&self, context: &EvaluatorStepContext<'_>) -> BuiltinTaskPoll {
        match self.operation {
            ListWalkOperation::Len => BuiltinTaskPoll::Ready(
                context.root_value(Value::Number(Number::from_usize(self.position))),
            ),
            ListWalkOperation::SplitEnd { .. } => {
                unreachable!("split-end empty results are handled by back traversal")
            }
            ListWalkOperation::At { .. } => {
                failure(context, "list at builtin index is out of bounds")
            }
            ListWalkOperation::Head => failure(
                context,
                "list head builtin requires a non-empty list or binary",
            ),
            ListWalkOperation::Tail => failure(
                context,
                "list tail builtin requires a non-empty list or binary",
            ),
            ListWalkOperation::Split { .. } => {
                failure(context, "split builtin index is out of bounds")
            }
            ListWalkOperation::Slice { .. } => {
                failure(context, "slice builtin end is out of bounds")
            }
        }
    }
}

enum DemandPoll {
    Ready(RuntimeValueRoot),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl DemandPoll {
    fn into_builtin_poll(self) -> BuiltinTaskPoll {
        match self {
            Self::Ready(_) => unreachable!("a ready demand must be consumed by its owner"),
            Self::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
            Self::Yielded => BuiltinTaskPoll::Yielded,
            Self::Failed(failure) => BuiltinTaskPoll::Failed(failure),
        }
    }
}

fn poll_demand(
    demand: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    durable_context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> DemandPoll {
    match poll_whnf_computation(demand, poll_context, durable_context, step_budget) {
        WhnfOwnerPoll::Ready(value) => DemandPoll::Ready(value),
        WhnfOwnerPoll::Pending(dependency) => DemandPoll::Pending(dependency),
        WhnfOwnerPoll::Yielded => DemandPoll::Yielded,
        WhnfOwnerPoll::Failed(failure) => DemandPoll::Failed(failure),
        WhnfOwnerPoll::External(boundary) => {
            unreachable!("list observation produced an external {boundary:?} boundary")
        }
    }
}

fn immediate_empty_prefix_result(
    context: &EvaluatorStepContext<'_>,
    operation: &ListWalkOperation,
    source: &RuntimeValueRoot,
) -> Option<BuiltinTaskPoll> {
    match operation {
        ListWalkOperation::Split { index: 0 } => {
            Some(rooted_split_from_items_and_root(context, &[], source))
        }
        ListWalkOperation::SplitEnd { count: 0 } => {
            Some(rooted_split_from_root_and_items(context, source, &[]))
        }
        ListWalkOperation::Slice { end: 0, .. } => Some(rooted_list_from_items(context, &[])),
        _ => None,
    }
}

fn rooted_list_from_items(
    context: &EvaluatorStepContext<'_>,
    items: &[RuntimeValueRoot],
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        let items = items.iter().map(|item| access.clone_root(item)).collect();
        access
            .values()
            .root_runtime_value(Value::List(List::from_values(items)))
    });
    BuiltinTaskPoll::Ready(result)
}

fn rooted_split_from_items_and_root(
    context: &EvaluatorStepContext<'_>,
    left: &[RuntimeValueRoot],
    right: &RuntimeValueRoot,
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        let left = left.iter().map(|item| access.clone_root(item)).collect();
        let right = access.clone_root(right);
        access.values().root_runtime_value(split_result_value(
            access.values(),
            Value::List(List::from_values(left)),
            right,
        ))
    });
    BuiltinTaskPoll::Ready(result)
}

fn rooted_split_from_root_and_items(
    context: &EvaluatorStepContext<'_>,
    left: &RuntimeValueRoot,
    right: &[RuntimeValueRoot],
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        let left = access.clone_root(left);
        let right = right.iter().map(|item| access.clone_root(item)).collect();
        access.values().root_runtime_value(split_result_value(
            access.values(),
            left,
            Value::List(List::from_values(right)),
        ))
    });
    BuiltinTaskPoll::Ready(result)
}

fn rooted_binary_split(
    context: &EvaluatorStepContext<'_>,
    bytes: &Bytes,
    index: usize,
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        access.values().root_runtime_value(split_result_value(
            access.values(),
            Value::Binary(bytes.slice(0..index)),
            Value::Binary(bytes.slice(index..bytes.len())),
        ))
    });
    BuiltinTaskPoll::Ready(result)
}

fn contextual_index_failure(
    context: &EvaluatorStepContext<'_>,
    failure: RuntimeFailureRoot,
    operation: &str,
) -> RuntimeFailureRoot {
    let failure = context.with_value_access(|access| {
        EvaluationHalt::failure(failure.into_failure()).with_context(
            access.values(),
            evaluation_context_frame_in(access.values(), operation),
        )
    });
    context.root_failure(failure.into_permanent_failure())
}

fn failure(context: &EvaluatorStepContext<'_>, message: &str) -> BuiltinTaskPoll {
    BuiltinTaskPoll::Failed(context.root_failure(Arc::new(EvaluationFailure::message(message))))
}
