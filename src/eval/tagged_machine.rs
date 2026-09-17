//! Resumable singleton-tag recognition and semantic-undefined traversal.
//!
//! Tagged dictionaries may contain ignored members only when those members
//! recursively evaluate to undefined dictionaries. This owner retains the
//! exact dictionary cursor and nested demand across polls so comparison,
//! application, and later builtin families share one interpretation.

use crate::core::{Dict, Key, RuntimeValueAccess, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, WorkDependency,
    poll_whnf_computation,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::whnf::WhnfComputation;

pub(crate) enum TaggedPayloadPoll {
    Ready(Option<RuntimeValueRoot>),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

/// Recognizes one dictionary tag while preserving recursive undefined work.
pub(crate) struct TaggedPayloadMachine {
    payload: Option<RuntimeValueRoot>,
    ignored: Vec<RuntimeValueRoot>,
    next_ignored: usize,
    checking_payload: bool,
    undefined: Option<SemanticUndefinedMachine>,
}

impl TaggedPayloadMachine {
    pub(crate) fn new(access: &RuntimeValueAccess<'_>, dict: &Dict, tag: &Key) -> Self {
        let payload = dict
            .get(tag)
            .map(|value| access.root_runtime_value(access.duplicate_value(value)));
        let ignored = dict
            .iter()
            .filter(|(key, _)| *key != tag)
            .map(|(_, value)| access.root_runtime_value(access.duplicate_value(value)))
            .collect();
        Self {
            payload,
            ignored,
            next_ignored: 0,
            checking_payload: true,
            undefined: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> TaggedPayloadPoll {
        let Some(payload) = &self.payload else {
            return TaggedPayloadPoll::Ready(None);
        };

        if self.undefined.is_none() {
            let candidate = if self.checking_payload {
                payload.clone()
            } else if let Some(value) = self.ignored.get(self.next_ignored) {
                self.next_ignored += 1;
                value.clone()
            } else {
                return TaggedPayloadPoll::Ready(Some(payload.clone()));
            };
            self.undefined = Some(SemanticUndefinedMachine::new(candidate));
            return TaggedPayloadPoll::Yielded;
        }

        match self
            .undefined
            .as_mut()
            .expect("semantic-undefined child must be installed")
            .poll(poll_context, context, durable_context, step_budget)
        {
            SemanticUndefinedPoll::Ready(undefined) => {
                self.undefined = None;
                if self.checking_payload {
                    self.checking_payload = false;
                    if undefined {
                        TaggedPayloadPoll::Ready(None)
                    } else {
                        TaggedPayloadPoll::Yielded
                    }
                } else if undefined {
                    TaggedPayloadPoll::Yielded
                } else {
                    TaggedPayloadPoll::Ready(None)
                }
            }
            SemanticUndefinedPoll::Pending(dependency) => TaggedPayloadPoll::Pending(dependency),
            SemanticUndefinedPoll::Yielded => TaggedPayloadPoll::Yielded,
            SemanticUndefinedPoll::Failed(failure) => TaggedPayloadPoll::Failed(failure),
        }
    }
}

pub(crate) enum SemanticUndefinedPoll {
    Ready(bool),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

/// A depth-first semantic-undefined walk with no Rust recursion.
pub(crate) struct SemanticUndefinedMachine {
    remaining: Vec<RuntimeValueRoot>,
    demand: Option<WhnfComputation>,
}

impl SemanticUndefinedMachine {
    pub(crate) fn new(value: RuntimeValueRoot) -> Self {
        Self {
            remaining: vec![value],
            demand: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> SemanticUndefinedPoll {
        if self.demand.is_none() {
            let Some(value) = self.remaining.pop() else {
                return SemanticUndefinedPoll::Ready(true);
            };
            self.demand = Some(WhnfComputation::from_root(value));
        }

        let value = match poll_whnf_computation(
            self.demand
                .as_mut()
                .expect("undefined traversal demand must be installed"),
            poll_context,
            durable_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(value) => value,
            WhnfOwnerPoll::Pending(dependency) => {
                return SemanticUndefinedPoll::Pending(dependency);
            }
            WhnfOwnerPoll::Yielded => return SemanticUndefinedPoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => return SemanticUndefinedPoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("semantic-undefined demand produced an external {boundary:?} boundary")
            }
        };
        self.demand = None;

        let members = context.with_value_access(|access| match access.clone_root(&value) {
            Value::Dict(dict) => Some(
                dict.iter()
                    .map(|(_, value)| {
                        access
                            .values()
                            .root_runtime_value(access.values().duplicate_value(value))
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        });
        let Some(members) = members else {
            return SemanticUndefinedPoll::Ready(false);
        };
        self.remaining.extend(members.into_iter().rev());
        if self.remaining.is_empty() {
            SemanticUndefinedPoll::Ready(true)
        } else {
            SemanticUndefinedPoll::Yielded
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CoreValueFactory, PromisedValue};
    use crate::evaluation::EvaluationStepBudget;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    fn poll(machine: &mut TaggedPayloadMachine, context: &EvalContext) -> TaggedPayloadPoll {
        let poll = EvaluationPollContext::for_context(context);
        poll.evaluate(context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                context,
                &mut EvaluationStepBudget::new(64),
            )
        })
    }

    #[test]
    fn tagged_payload_resumes_nested_undefined_work_at_the_exact_member() {
        let context = context();
        let tag = Key::atom_from_text("tuple");
        let ignored = Key::atom_from_text("ignored");
        let nested = Key::atom_from_text("nested");
        let promise = PromisedValue::new(context.values(), "nested undefined member");
        let dict = Dict::new_sync()
            .insert(tag.clone(), Value::Number(7.into()))
            .insert(
                ignored,
                Value::Dict(Dict::new_sync().insert(nested, Value::Promised(promise.clone()))),
            );
        let mut machine = context
            .values()
            .with_runtime_value_access(|access| TaggedPayloadMachine::new(&access, &dict, &tag));

        loop {
            match poll(&mut machine, &context) {
                TaggedPayloadPoll::Yielded => {}
                TaggedPayloadPoll::Pending(_) => break,
                TaggedPayloadPoll::Ready(_) => {
                    panic!("nested promise must suspend tag recognition")
                }
                TaggedPayloadPoll::Failed(error) => panic!("tag recognition failed: {error}"),
            }
        }

        crate::core::set_test_promise(context.values(), &promise, Value::Dict(Dict::new_sync()))
            .expect("nested undefined promise should accept its assignment");
        let payload = loop {
            match poll(&mut machine, &context) {
                TaggedPayloadPoll::Yielded => {}
                TaggedPayloadPoll::Ready(Some(payload)) => break payload,
                TaggedPayloadPoll::Ready(None) => {
                    panic!("undefined ignored member must preserve the tag")
                }
                TaggedPayloadPoll::Pending(_) => panic!("assigned nested member must resume"),
                TaggedPayloadPoll::Failed(error) => panic!("tag recognition failed: {error}"),
            }
        };
        assert_eq!(payload.clone_core_for_test(), Value::Number(7.into()));
    }
}
