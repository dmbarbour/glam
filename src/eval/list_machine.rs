//! Pollable logical-list projection shared by source-owned computations.
//!
//! The persistent list representation itself performs only non-forcing
//! decomposition. This owner retains the exact deferred chunk and logical
//! suffix while ordinary WHNF orchestration resolves that chunk.

use crate::core::{EvaluationHalt, LazyId, List, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::list::{ListFrontStep, ListItem};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::whnf::WhnfComputation;

pub(crate) enum ListFrontPoll {
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
        step_budget: usize,
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
            list.pop_front_step_by(
                &mut |value| access.values().duplicate_value(value),
                &mut |thunk| thunk.duplicate_as_value_in(access.values()),
            )
        });
        match step {
            ListFrontStep::Empty => ListFrontPoll::Ready(None),
            ListFrontStep::Item { item, tail } => {
                let value = match item {
                    ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
                    ListItem::Value(value) => value,
                };
                ListFrontPoll::Ready(Some((
                    context.root_value(value),
                    context.root_value(Value::List(tail)),
                )))
            }
            ListFrontStep::Deferred { deferred, suffix } => {
                let mut chunk = WhnfComputation::from_root(context.root_value(deferred));
                if let Some(source_owner) = self.source_owner {
                    chunk = chunk.with_source_owner(source_owner);
                }
                self.chunk = Some(chunk);
                self.suffix = Some(context.root_value(Value::List(suffix)));
                ListFrontPoll::Yielded
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
