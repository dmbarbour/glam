//! Resumable singleton-tag recognition and semantic-undefined traversal.
//!
//! Tagged dictionaries may contain ignored members only when those members
//! recursively evaluate to undefined dictionaries. This owner retains the
//! exact dictionary cursor and nested demand across polls so comparison,
//! application, and later builtin families share one interpretation.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Dict, EvaluatedValue, EvaluationFailure, Key, LazyId, Value,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::EvaluationValueAccess;

use super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place,
    reduce_semantic_shell,
};

pub(in crate::eval) enum RegionalTaggedPayloadPoll {
    Ready(Option<Value>),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

/// Raw tagged-dictionary recognition retained beneath a managed parent.
pub(in crate::eval) struct RegionalTaggedPayload {
    payload: Option<Value>,
    ignored: Vec<Value>,
    next_ignored: usize,
    checking_payload: bool,
    undefined: Option<RegionalSemanticUndefined>,
    source_owner: LazyId,
}

pub(in crate::eval) enum RegionalSemanticUndefinedPoll {
    Ready(bool),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

/// Depth-first semantic-undefined traversal beneath a managed parent.
pub(in crate::eval) struct RegionalSemanticUndefined {
    remaining: Vec<Value>,
    demand: Option<RegionalWhnfWork>,
    source_owner: LazyId,
}

impl RegionalTaggedPayload {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        dict: &Dict,
        tag: &Key,
        source_owner: LazyId,
    ) -> Self {
        let payload = dict
            .get(tag)
            .map(|value| access.values().duplicate_value(value));
        let ignored = dict
            .iter()
            .filter(|(key, _)| *key != tag)
            .map(|(_, value)| access.values().duplicate_value(value))
            .collect();
        Self {
            payload,
            ignored,
            next_ignored: 0,
            checking_payload: true,
            undefined: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalTaggedPayloadPoll {
        let Some(payload) = &self.payload else {
            return RegionalTaggedPayloadPoll::Ready(None);
        };

        if self.undefined.is_none() {
            let candidate = if self.checking_payload {
                access.values().duplicate_value(payload)
            } else if let Some(value) = self.ignored.get(self.next_ignored) {
                self.next_ignored += 1;
                access.values().duplicate_value(value)
            } else {
                return RegionalTaggedPayloadPoll::Ready(Some(
                    access.values().duplicate_value(payload),
                ));
            };
            self.undefined = Some(RegionalSemanticUndefined::new_in(
                access,
                candidate,
                self.source_owner,
            ));
            return RegionalTaggedPayloadPoll::Yielded;
        }

        match self
            .undefined
            .as_mut()
            .expect("semantic-undefined child must be installed")
            .poll_in(access, step_budget)
        {
            RegionalSemanticUndefinedPoll::Ready(undefined) => {
                self.undefined = None;
                if self.checking_payload {
                    self.checking_payload = false;
                    if undefined {
                        RegionalTaggedPayloadPoll::Ready(None)
                    } else {
                        RegionalTaggedPayloadPoll::Yielded
                    }
                } else if undefined {
                    RegionalTaggedPayloadPoll::Yielded
                } else {
                    RegionalTaggedPayloadPoll::Ready(None)
                }
            }
            RegionalSemanticUndefinedPoll::Boundary(request) => {
                RegionalTaggedPayloadPoll::Boundary(request)
            }
            RegionalSemanticUndefinedPoll::Yielded => RegionalTaggedPayloadPoll::Yielded,
            RegionalSemanticUndefinedPoll::Failed(failure) => {
                RegionalTaggedPayloadPoll::Failed(failure)
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(payload) = &self.payload {
            trace_compatibility_value_managed_edges(payload, visitor);
        }
        for value in &self.ignored {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(undefined) = &self.undefined {
            undefined.trace_managed_edges(visitor);
        }
    }
}

impl RegionalSemanticUndefined {
    pub(in crate::eval) fn new_in(
        _access: &EvaluationValueAccess<'_>,
        value: Value,
        source_owner: LazyId,
    ) -> Self {
        Self {
            remaining: vec![value],
            demand: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalSemanticUndefinedPoll {
        if self.demand.is_none() {
            let Some(value) = self.remaining.pop() else {
                return RegionalSemanticUndefinedPoll::Ready(true);
            };
            self.demand = Some(
                RegionalWhnfWork::from_focus(access, value).with_source_owner(self.source_owner),
            );
        }

        let value = match drive_regional_in_place(
            access,
            self.demand
                .as_mut()
                .expect("undefined traversal demand must be installed"),
            step_budget,
            reduce_semantic_shell,
        ) {
            RegionalWhnfStatus::Ready(value) => EvaluatedValue::try_from(value)
                .expect("semantic-undefined demand must reach WHNF")
                .into_value(),
            RegionalWhnfStatus::Boundary(request) => {
                return RegionalSemanticUndefinedPoll::Boundary(request);
            }
            RegionalWhnfStatus::Yielded => return RegionalSemanticUndefinedPoll::Yielded,
            RegionalWhnfStatus::Failed(failure) => {
                return RegionalSemanticUndefinedPoll::Failed(failure);
            }
        };
        self.demand = None;

        let Value::Dict(dict) = value else {
            return RegionalSemanticUndefinedPoll::Ready(false);
        };
        self.remaining.extend(
            dict.iter()
                .rev()
                .map(|(_, value)| access.values().duplicate_value(value)),
        );
        if self.remaining.is_empty() {
            RegionalSemanticUndefinedPoll::Ready(true)
        } else {
            RegionalSemanticUndefinedPoll::Yielded
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for value in &self.remaining {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CoreValueFactory, PromisedValue};
    use crate::evaluation::{EvalContext, EvaluationPollContext, EvaluationStepBudget};
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    fn poll(
        machine: &mut RegionalTaggedPayload,
        context: &EvalContext,
    ) -> RegionalTaggedPayloadPoll {
        let poll = EvaluationPollContext::for_context(context);
        poll.with_value_access(context, |access| {
            machine.poll_in(&access, &mut EvaluationStepBudget::new(64))
        })
    }

    fn recognize(context: &EvalContext, dict: &Dict, tag: &Key) -> Option<Value> {
        let (owner, _) = context
            .values()
            .rooted_error_lazy_for_test("regional tagged-payload owner");
        let poll_context = EvaluationPollContext::for_context(context);
        let mut machine = poll_context.with_value_access(context, |access| {
            RegionalTaggedPayload::new_in(&access, dict, tag, owner.id())
        });
        loop {
            match poll(&mut machine, context) {
                RegionalTaggedPayloadPoll::Ready(payload) => return payload,
                RegionalTaggedPayloadPoll::Yielded => {}
                RegionalTaggedPayloadPoll::Boundary(_) => {
                    panic!("strict tagged-payload recognition must not suspend")
                }
                RegionalTaggedPayloadPoll::Failed(error) => {
                    panic!("closed tagged-payload recognition failed: {error}")
                }
            }
        }
    }

    #[test]
    fn tagged_payload_ignores_only_semantically_undefined_extra_entries() {
        let context = context();
        let tag = Key::atom_from_text("tuple");
        let payload = Value::Number(42.into());
        let recursively_empty = Value::Dict(
            Dict::new_sync().insert(Key::atom_from_text("nested"), Value::Dict(Dict::new_sync())),
        );
        let tagged = Dict::new_sync()
            .insert(tag.clone(), payload.clone())
            .insert(Key::atom_from_text("ignored"), recursively_empty.clone());

        assert_eq!(recognize(&context, &tagged, &tag), Some(payload));
        assert_eq!(
            recognize(
                &context,
                &tagged.insert(Key::atom_from_text("defined"), Value::Number(1.into())),
                &tag,
            ),
            None
        );
        assert_eq!(
            recognize(
                &context,
                &Dict::new_sync().insert(tag.clone(), recursively_empty),
                &tag,
            ),
            None
        );
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
        let (owner, _) = context
            .values()
            .rooted_error_lazy_for_test("regional tagged-payload owner");
        let poll_context = EvaluationPollContext::for_context(&context);
        let mut machine = poll_context.with_value_access(&context, |access| {
            RegionalTaggedPayload::new_in(&access, &dict, &tag, owner.id())
        });

        loop {
            match poll(&mut machine, &context) {
                RegionalTaggedPayloadPoll::Yielded => {}
                RegionalTaggedPayloadPoll::Boundary(_) => break,
                RegionalTaggedPayloadPoll::Ready(_) => {
                    panic!("nested promise must suspend tag recognition")
                }
                RegionalTaggedPayloadPoll::Failed(error) => {
                    panic!("tag recognition failed: {error}")
                }
            }
        }

        crate::core::set_test_promise(context.values(), &promise, Value::Dict(Dict::new_sync()))
            .expect("nested undefined promise should accept its assignment");
        let payload = loop {
            match poll(&mut machine, &context) {
                RegionalTaggedPayloadPoll::Yielded => {}
                RegionalTaggedPayloadPoll::Ready(Some(payload)) => break payload,
                RegionalTaggedPayloadPoll::Ready(None) => {
                    panic!("undefined ignored member must preserve the tag")
                }
                RegionalTaggedPayloadPoll::Boundary(_) => {
                    panic!("assigned nested member must resume")
                }
                RegionalTaggedPayloadPoll::Failed(error) => {
                    panic!("tag recognition failed: {error}")
                }
            }
        };
        assert_eq!(payload, Value::Number(7.into()));
    }
}
