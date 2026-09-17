//! Pollable logical-list projection shared by source-owned computations.
//!
//! The persistent list representation itself performs only non-forcing
//! decomposition. This owner retains the exact deferred chunk and logical
//! suffix while ordinary WHNF orchestration resolves that chunk.

use crate::core::{EvaluationHalt, LazyId, List, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::list::{ListBackStep, ListFrontStep, ListItem};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::whnf::WhnfComputation;

pub(crate) enum ListFrontPoll {
    Ready(Option<(RuntimeValueRoot, RuntimeValueRoot)>),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) enum ListBackPoll {
    Ready(Option<(RuntimeValueRoot, RuntimeValueRoot)>),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) struct ListFrontMachine {
    current: RuntimeValueRoot,
    chunk: Option<WhnfComputation>,
    suffix: Option<RuntimeValueRoot>,
    source_owner: Option<LazyId>,
}

enum RootedListFrontStep {
    Empty,
    Item {
        item: RuntimeValueRoot,
        tail: RuntimeValueRoot,
    },
    Deferred {
        deferred: RuntimeValueRoot,
        suffix: RuntimeValueRoot,
    },
}

pub(crate) struct ListBackMachine {
    current: RuntimeValueRoot,
    chunk: Option<WhnfComputation>,
    prefix: Option<RuntimeValueRoot>,
}

enum RootedListBackStep {
    Empty,
    Item {
        init: RuntimeValueRoot,
        item: RuntimeValueRoot,
    },
    Deferred {
        prefix: RuntimeValueRoot,
        deferred: RuntimeValueRoot,
    },
}

impl ListFrontMachine {
    pub(super) fn new(list: RuntimeValueRoot, source_owner: LazyId) -> Self {
        Self {
            current: list,
            chunk: None,
            suffix: None,
            source_owner: Some(source_owner),
        }
    }

    pub(crate) fn unowned(list: RuntimeValueRoot) -> Self {
        Self {
            current: list,
            chunk: None,
            suffix: None,
            source_owner: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ListFrontPoll {
        if let Some(chunk) = &mut self.chunk {
            let value =
                match poll_whnf_computation(chunk, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(value) => value,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return ListFrontPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return ListFrontPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return ListFrontPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("lazy list chunk produced an external {boundary:?} boundary")
                    }
                };
            let suffix = self
                .suffix
                .take()
                .expect("deferred list work must retain its exact suffix");
            self.current = match combine_chunk_and_suffix(context, value, suffix) {
                Ok(list) => list,
                Err(error) => return ListFrontPoll::Failed(context.root_failure(error)),
            };
            self.chunk = None;
            return ListFrontPoll::Yielded;
        }

        let step = context.with_value_access(|access| {
            let Value::List(list) = access.clone_root(&self.current) else {
                unreachable!("logical-list-front work must retain a list value")
            };
            match list.pop_front_step_by(
                &mut |value| access.values().duplicate_value(value),
                &mut |thunk| thunk.duplicate_as_value_in(access.values()),
            ) {
                ListFrontStep::Empty => RootedListFrontStep::Empty,
                ListFrontStep::Item { item, tail } => {
                    let value = match item {
                        ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
                        ListItem::Value(value) => value,
                    };
                    RootedListFrontStep::Item {
                        item: access.values().root_runtime_value(value),
                        tail: access.values().root_runtime_value(Value::List(tail)),
                    }
                }
                ListFrontStep::Deferred { deferred, suffix } => RootedListFrontStep::Deferred {
                    deferred: access.values().root_runtime_value(deferred),
                    suffix: access.values().root_runtime_value(Value::List(suffix)),
                },
            }
        });
        match step {
            RootedListFrontStep::Empty => ListFrontPoll::Ready(None),
            RootedListFrontStep::Item { item, tail } => ListFrontPoll::Ready(Some((item, tail))),
            RootedListFrontStep::Deferred { deferred, suffix } => {
                let mut chunk = WhnfComputation::from_root(deferred);
                if let Some(source_owner) = self.source_owner {
                    chunk = chunk.with_source_owner(source_owner);
                }
                self.chunk = Some(chunk);
                self.suffix = Some(suffix);
                ListFrontPoll::Yielded
            }
        }
    }
}

impl ListBackMachine {
    pub(crate) fn new(list: RuntimeValueRoot) -> Self {
        Self {
            current: list,
            chunk: None,
            prefix: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ListBackPoll {
        if let Some(chunk) = &mut self.chunk {
            let value =
                match poll_whnf_computation(chunk, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(value) => value,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return ListBackPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return ListBackPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return ListBackPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("lazy list chunk produced an external {boundary:?} boundary")
                    }
                };
            let prefix = self
                .prefix
                .take()
                .expect("deferred back-list work must retain its exact prefix");
            self.current = match combine_prefix_and_chunk(context, prefix, value) {
                Ok(list) => list,
                Err(error) => return ListBackPoll::Failed(context.root_failure(error)),
            };
            self.chunk = None;
            return ListBackPoll::Yielded;
        }

        let step = context.with_value_access(|access| {
            let Value::List(list) = access.clone_root(&self.current) else {
                unreachable!("logical-list-back work must retain a list value")
            };
            match list.pop_back_step_by(
                &mut |value| access.values().duplicate_value(value),
                &mut |thunk| thunk.duplicate_as_value_in(access.values()),
            ) {
                ListBackStep::Empty => RootedListBackStep::Empty,
                ListBackStep::Item { init, item } => {
                    let value = match item {
                        ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
                        ListItem::Value(value) => value,
                    };
                    RootedListBackStep::Item {
                        init: access.values().root_runtime_value(Value::List(init)),
                        item: access.values().root_runtime_value(value),
                    }
                }
                ListBackStep::Deferred { prefix, deferred } => RootedListBackStep::Deferred {
                    prefix: access.values().root_runtime_value(Value::List(prefix)),
                    deferred: access.values().root_runtime_value(deferred),
                },
            }
        });
        match step {
            RootedListBackStep::Empty => ListBackPoll::Ready(None),
            RootedListBackStep::Item { init, item } => ListBackPoll::Ready(Some((init, item))),
            RootedListBackStep::Deferred { prefix, deferred } => {
                self.chunk = Some(WhnfComputation::from_root(deferred));
                self.prefix = Some(prefix);
                ListBackPoll::Yielded
            }
        }
    }
}

fn combine_chunk_and_suffix(
    context: &EvaluatorStepContext<'_>,
    chunk: RuntimeValueRoot,
    suffix: RuntimeValueRoot,
) -> Result<RuntimeValueRoot, std::sync::Arc<crate::core::EvaluationFailure>> {
    context.with_value_access(|access| {
        let chunk = match access.clone_root(&chunk) {
            Value::Binary(bytes) => List::from_bytes(bytes),
            Value::List(list) => list,
            other => {
                return Err(EvaluationHalt::new(format!(
                    "lazy list chunk must evaluate to a list or binary value, got {other:?}"
                ))
                .into_permanent_failure());
            }
        };
        let Value::List(suffix) = access.clone_root(&suffix) else {
            unreachable!("logical-list-front work must retain a list suffix")
        };
        Ok(access
            .values()
            .root_runtime_value(Value::List(List::concat(chunk, suffix))))
    })
}

fn combine_prefix_and_chunk(
    context: &EvaluatorStepContext<'_>,
    prefix: RuntimeValueRoot,
    chunk: RuntimeValueRoot,
) -> Result<RuntimeValueRoot, std::sync::Arc<crate::core::EvaluationFailure>> {
    context.with_value_access(|access| {
        let Value::List(prefix) = access.clone_root(&prefix) else {
            unreachable!("logical-list-back work must retain a list prefix")
        };
        let chunk = match access.clone_root(&chunk) {
            Value::Binary(bytes) => List::from_bytes(bytes),
            Value::List(list) => list,
            other => {
                return Err(EvaluationHalt::new(format!(
                    "lazy list chunk must evaluate to a list or binary value, got {other:?}"
                ))
                .into_permanent_failure());
            }
        };
        Ok(access
            .values()
            .root_runtime_value(Value::List(List::concat(prefix, chunk))))
    })
}
