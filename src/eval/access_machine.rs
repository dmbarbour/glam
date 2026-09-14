//! Resumable computed dictionary access and recursive key conversion.
//!
//! This is source-owned semantic state, not a second evaluator. Every value
//! which crosses a poll boundary is a runtime root; ordinary WHNF demand and
//! dependency admission remain delegated to `WhnfComputation` and its
//! scheduler adapter.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::core::{
    Dict, EvaluationFailure, EvaluationHalt, Key, LazyId, List, RuntimeValueAccess, Value,
};
use crate::core_net::CoreDataKey;
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::list::{ListFrontStep, ListItem};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::whnf::WhnfComputation;

pub(super) enum AccessMachinePoll {
    Ready(RuntimeValueRoot),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(super) struct AccessMachine {
    path: Arc<[CoreDataKey]>,
    arguments: Vec<RuntimeValueRoot>,
    next_part: usize,
    next_argument: usize,
    pending_keys: VecDeque<Key>,
    current: RuntimeValueRoot,
    demand: Option<WhnfComputation>,
    conversion: Option<AccessConversion>,
    source_owner: LazyId,
}

enum AccessConversion {
    Key(KeyConversionMachine),
    Path(Box<KeyListMachine>),
}

pub(crate) enum ConversionPoll<T> {
    Ready(T),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) struct KeyConversionMachine {
    state: KeyConversionState,
    source_owner: Option<LazyId>,
}

enum KeyConversionState {
    Demand(WhnfComputation),
    Dict(DictConversion),
    List(Box<KeyListMachine>),
}

enum ClassifiedKeyValue {
    Ready(Key),
    Dict(Vec<(Key, RuntimeValueRoot)>),
    List(RuntimeValueRoot),
    Invalid,
}

struct DictConversion {
    members: Vec<(Key, RuntimeValueRoot)>,
    next: usize,
    converted: Vec<(Key, Key)>,
    child: Option<Box<KeyConversionMachine>>,
}

pub(crate) struct KeyListMachine {
    source: Option<WhnfComputation>,
    lists: Vec<RuntimeValueRoot>,
    chunk: Option<WhnfComputation>,
    chunk_suffix: Option<RuntimeValueRoot>,
    child: Option<Box<KeyConversionMachine>>,
    converted: Vec<Key>,
    source_owner: Option<LazyId>,
}

impl AccessMachine {
    pub(super) fn new(
        source_owner: LazyId,
        path: Arc<[CoreDataKey]>,
        arguments: Vec<RuntimeValueRoot>,
    ) -> Self {
        let current = arguments
            .first()
            .cloned()
            .expect("value access must retain its base value");
        Self {
            path,
            arguments,
            next_part: 0,
            next_argument: 1,
            pending_keys: VecDeque::new(),
            current,
            demand: None,
            conversion: None,
            source_owner,
        }
    }

    pub(super) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: usize,
    ) -> AccessMachinePoll {
        if let Some(conversion) = &mut self.conversion {
            let result = match conversion {
                AccessConversion::Key(machine) => {
                    machine.poll(poll_context, context, durable_context, step_budget)
                }
                AccessConversion::Path(machine) => {
                    return match machine.poll(poll_context, context, durable_context, step_budget) {
                        ConversionPoll::Ready(keys) => {
                            self.conversion = None;
                            self.next_part += 1;
                            self.begin_key_sequence(keys);
                            AccessMachinePoll::Yielded
                        }
                        ConversionPoll::Pending(dependency) => {
                            AccessMachinePoll::Pending(dependency)
                        }
                        ConversionPoll::Yielded => AccessMachinePoll::Yielded,
                        ConversionPoll::Failed(failure) => AccessMachinePoll::Failed(failure),
                    };
                }
            };
            return match result {
                ConversionPoll::Ready(key) => {
                    self.conversion = None;
                    self.next_part += 1;
                    self.begin_key_sequence([key]);
                    AccessMachinePoll::Yielded
                }
                ConversionPoll::Pending(dependency) => AccessMachinePoll::Pending(dependency),
                ConversionPoll::Yielded => AccessMachinePoll::Yielded,
                ConversionPoll::Failed(failure) => AccessMachinePoll::Failed(failure),
            };
        }

        if let Some(demand) = &mut self.demand {
            let current =
                match poll_whnf_computation(demand, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(value) => value,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return AccessMachinePoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return AccessMachinePoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return AccessMachinePoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("computed access produced an external {boundary:?} boundary")
                    }
                };
            self.demand = None;
            let Some(key) = self.pending_keys.pop_front() else {
                debug_assert_eq!(self.next_part, self.path.len());
                return AccessMachinePoll::Ready(current);
            };
            let selected = context
                .with_value_access(|access| select_dict_member(access.values(), &current, &key));
            return match selected {
                Ok(value) => {
                    self.current = value;
                    if !self.pending_keys.is_empty() {
                        self.demand = Some(
                            WhnfComputation::from_root(self.current.clone())
                                .with_source_owner(self.source_owner),
                        );
                    }
                    AccessMachinePoll::Yielded
                }
                Err(error) => AccessMachinePoll::Failed(root_halt(context, error)),
            };
        }

        let Some(part) = self.path.get(self.next_part) else {
            self.demand = Some(
                WhnfComputation::from_root(self.current.clone())
                    .with_source_owner(self.source_owner),
            );
            return AccessMachinePoll::Yielded;
        };
        match part {
            CoreDataKey::Key(key) => {
                self.next_part += 1;
                self.begin_key_sequence([key.clone()]);
            }
            CoreDataKey::Index => {
                let argument = self.next_dynamic_argument();
                self.conversion = Some(AccessConversion::Key(KeyConversionMachine::new(
                    argument,
                    Some(self.source_owner),
                )));
            }
            CoreDataKey::PathIndex => {
                let argument = self.next_dynamic_argument();
                self.conversion = Some(AccessConversion::Path(Box::new(KeyListMachine::new(
                    argument,
                    Some(self.source_owner),
                ))));
            }
        }
        AccessMachinePoll::Yielded
    }

    fn next_dynamic_argument(&mut self) -> RuntimeValueRoot {
        let argument = self
            .arguments
            .get(self.next_argument)
            .cloned()
            .expect("lowered access index must retain its argument");
        self.next_argument += 1;
        argument
    }

    fn begin_key_sequence(&mut self, keys: impl IntoIterator<Item = Key>) {
        self.pending_keys.extend(keys);
        if !self.pending_keys.is_empty() {
            self.demand = Some(
                WhnfComputation::from_root(self.current.clone())
                    .with_source_owner(self.source_owner),
            );
        }
    }
}

impl KeyConversionMachine {
    pub(crate) fn new(value: RuntimeValueRoot, source_owner: Option<LazyId>) -> Self {
        Self {
            state: KeyConversionState::Demand(owned_whnf(value, source_owner)),
            source_owner,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: usize,
    ) -> ConversionPoll<Key> {
        match &mut self.state {
            KeyConversionState::Demand(computation) => {
                let value = match poll_whnf_computation(
                    computation,
                    poll_context,
                    durable_context,
                    step_budget,
                ) {
                    WhnfOwnerPoll::Ready(value) => value,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return ConversionPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return ConversionPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => {
                        return ConversionPoll::Failed(failure);
                    }
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("key conversion produced an external {boundary:?} boundary")
                    }
                };
                match classify_key_value(context, &value) {
                    ClassifiedKeyValue::Ready(key) => ConversionPoll::Ready(key),
                    ClassifiedKeyValue::List(value) => {
                        self.state = KeyConversionState::List(Box::new(
                            KeyListMachine::from_ready(value, self.source_owner),
                        ));
                        ConversionPoll::Yielded
                    }
                    ClassifiedKeyValue::Dict(members) => {
                        self.state = KeyConversionState::Dict(DictConversion {
                            members,
                            next: 0,
                            converted: Vec::new(),
                            child: None,
                        });
                        ConversionPoll::Yielded
                    }
                    ClassifiedKeyValue::Invalid => ConversionPoll::Failed(root_message(
                        context,
                        "dictionary keys must evaluate to keyable values",
                    )),
                }
            }
            KeyConversionState::Dict(dict) => {
                if let Some(child) = &mut dict.child {
                    return match child.poll(poll_context, context, durable_context, step_budget) {
                        ConversionPoll::Ready(value) => {
                            let (key, _) = &dict.members[dict.next - 1];
                            if !matches!(&value, Key::Dict(entries) if entries.is_empty()) {
                                dict.converted.push((key.clone(), value));
                            }
                            dict.child = None;
                            ConversionPoll::Yielded
                        }
                        ConversionPoll::Pending(dependency) => ConversionPoll::Pending(dependency),
                        ConversionPoll::Yielded => ConversionPoll::Yielded,
                        ConversionPoll::Failed(failure) => ConversionPoll::Failed(failure),
                    };
                }
                let Some((_, value)) = dict.members.get(dict.next) else {
                    return ConversionPoll::Ready(Key::Dict(Arc::from(std::mem::take(
                        &mut dict.converted,
                    ))));
                };
                dict.next += 1;
                dict.child = Some(Box::new(KeyConversionMachine::new(
                    value.clone(),
                    self.source_owner,
                )));
                ConversionPoll::Yielded
            }
            KeyConversionState::List(list) => {
                match list.poll(poll_context, context, durable_context, step_budget) {
                    ConversionPoll::Ready(items) => {
                        ConversionPoll::Ready(Key::List(Arc::from(items)))
                    }
                    ConversionPoll::Pending(dependency) => ConversionPoll::Pending(dependency),
                    ConversionPoll::Yielded => ConversionPoll::Yielded,
                    ConversionPoll::Failed(failure) => ConversionPoll::Failed(failure),
                }
            }
        }
    }
}

impl KeyListMachine {
    fn new(value: RuntimeValueRoot, source_owner: Option<LazyId>) -> Self {
        Self {
            source: Some(owned_whnf(value, source_owner)),
            lists: Vec::new(),
            chunk: None,
            chunk_suffix: None,
            child: None,
            converted: Vec::new(),
            source_owner,
        }
    }

    fn from_ready(value: RuntimeValueRoot, source_owner: Option<LazyId>) -> Self {
        Self {
            source: None,
            lists: vec![value],
            chunk: None,
            chunk_suffix: None,
            child: None,
            converted: Vec::new(),
            source_owner,
        }
    }

    pub(crate) fn unowned(value: RuntimeValueRoot) -> Self {
        Self::new(value, None)
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: usize,
    ) -> ConversionPoll<Vec<Key>> {
        if let Some(child) = &mut self.child {
            return match child.poll(poll_context, context, durable_context, step_budget) {
                ConversionPoll::Ready(key) => {
                    self.converted.push(key);
                    self.child = None;
                    ConversionPoll::Yielded
                }
                ConversionPoll::Pending(dependency) => ConversionPoll::Pending(dependency),
                ConversionPoll::Yielded => ConversionPoll::Yielded,
                ConversionPoll::Failed(failure) => ConversionPoll::Failed(failure),
            };
        }

        if let Some(computation) = &mut self.source {
            let value = match poll_whnf_computation(
                computation,
                poll_context,
                durable_context,
                step_budget,
            ) {
                WhnfOwnerPoll::Ready(value) => value,
                WhnfOwnerPoll::Pending(dependency) => {
                    return ConversionPoll::Pending(dependency);
                }
                WhnfOwnerPoll::Yielded => return ConversionPoll::Yielded,
                WhnfOwnerPoll::Failed(failure) => return ConversionPoll::Failed(failure),
                WhnfOwnerPoll::External(boundary) => {
                    unreachable!("path-list source produced an external {boundary:?} boundary")
                }
            };
            let list = match value_as_list_root(context, value, "path-list operand", false) {
                Ok(list) => list,
                Err(error) => return ConversionPoll::Failed(root_halt(context, error)),
            };
            self.source = None;
            self.lists.push(list);
            return ConversionPoll::Yielded;
        }

        if let Some(computation) = &mut self.chunk {
            let value = match poll_whnf_computation(
                computation,
                poll_context,
                durable_context,
                step_budget,
            ) {
                WhnfOwnerPoll::Ready(value) => value,
                WhnfOwnerPoll::Pending(dependency) => {
                    return ConversionPoll::Pending(dependency);
                }
                WhnfOwnerPoll::Yielded => return ConversionPoll::Yielded,
                WhnfOwnerPoll::Failed(failure) => return ConversionPoll::Failed(failure),
                WhnfOwnerPoll::External(boundary) => {
                    unreachable!("lazy list chunk produced an external {boundary:?} boundary")
                }
            };
            let list = match value_as_list_root(context, value, "lazy list chunk", true) {
                Ok(list) => list,
                Err(error) => return ConversionPoll::Failed(root_halt(context, error)),
            };
            self.chunk = None;
            if let Some(suffix) = self.chunk_suffix.take() {
                self.lists.push(suffix);
            }
            self.lists.push(list);
            return ConversionPoll::Yielded;
        }

        let Some(list) = self.lists.pop() else {
            return ConversionPoll::Ready(std::mem::take(&mut self.converted));
        };
        let step = context.with_value_access(|access| {
            let Value::List(list) = access.clone_root(&list) else {
                unreachable!("key-list work must retain list roots")
            };
            list.pop_front_step_by(
                &mut |value| access.values().duplicate_value(value),
                &mut |thunk| thunk.duplicate_as_value_in(access.values()),
            )
        });
        match step {
            ListFrontStep::Empty => ConversionPoll::Yielded,
            ListFrontStep::Item { item, tail } => {
                self.lists.push(context.root_value(Value::List(tail)));
                match item {
                    ListItem::Byte(byte) => {
                        self.converted.push(Key::Number(Number::from_u8(byte)));
                    }
                    ListItem::Value(value) => {
                        self.child = Some(Box::new(KeyConversionMachine::new(
                            context.root_value(value),
                            self.source_owner,
                        )));
                    }
                }
                ConversionPoll::Yielded
            }
            ListFrontStep::Deferred { deferred, suffix } => {
                self.chunk = Some(owned_whnf(context.root_value(deferred), self.source_owner));
                self.chunk_suffix = Some(context.root_value(Value::List(suffix)));
                ConversionPoll::Yielded
            }
        }
    }
}

fn owned_whnf(value: RuntimeValueRoot, source_owner: Option<LazyId>) -> WhnfComputation {
    let computation = WhnfComputation::from_root(value);
    match source_owner {
        Some(source_owner) => computation.with_source_owner(source_owner),
        None => computation,
    }
}

fn select_dict_member(
    access: &RuntimeValueAccess<'_>,
    current: &RuntimeValueRoot,
    key: &Key,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    let Value::Dict(dict) = current.clone_core_with(access) else {
        return Err(EvaluationHalt::new("value access base is not a dictionary"));
    };
    Ok(access.root_runtime_value(
        dict.get(key)
            .map(|value| access.duplicate_value(value))
            .unwrap_or_else(|| Value::Dict(Dict::new_sync())),
    ))
}

fn classify_key_value(
    context: &EvaluatorStepContext<'_>,
    value: &RuntimeValueRoot,
) -> ClassifiedKeyValue {
    context.with_value_access(|access| match access.clone_root(value) {
        Value::Atom(atom) => ClassifiedKeyValue::Ready(Key::Atom(atom)),
        Value::Number(number) => ClassifiedKeyValue::Ready(Key::Number(number)),
        Value::Binary(bytes) => ClassifiedKeyValue::Ready(Key::Binary(bytes)),
        Value::List(_) => ClassifiedKeyValue::List(value.clone()),
        Value::Dict(dict) => ClassifiedKeyValue::Dict(
            dict.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        access
                            .values()
                            .root_runtime_value(access.values().duplicate_value(value)),
                    )
                })
                .collect(),
        ),
        Value::Builtin(_)
        | Value::PartialBuiltin(_)
        | Value::Function(_)
        | Value::Net(_)
        | Value::Lazy(_)
        | Value::Promised(_)
        | Value::Metadata(_)
        | Value::Opaque(_) => ClassifiedKeyValue::Invalid,
    })
}

fn value_as_list_root(
    context: &EvaluatorStepContext<'_>,
    value: RuntimeValueRoot,
    subject: &str,
    allow_binary: bool,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    context.with_value_access(|access| match access.clone_root(&value) {
        Value::Binary(bytes) if allow_binary => Ok(access
            .values()
            .root_runtime_value(Value::List(List::from_bytes(bytes)))),
        Value::List(_) => Ok(value),
        _other if subject == "path-list operand" => Err(EvaluationHalt::new(
            "path-list operand must evaluate to a list value",
        )),
        other => Err(EvaluationHalt::new(format!(
            "lazy list chunk must evaluate to a list or binary value, got {other:?}"
        ))),
    })
}

fn root_halt(context: &EvaluatorStepContext<'_>, halt: EvaluationHalt) -> RuntimeFailureRoot {
    context.root_failure(halt.into_permanent_failure())
}

fn root_message(context: &EvaluatorStepContext<'_>, message: &str) -> RuntimeFailureRoot {
    context.root_failure(Arc::new(EvaluationFailure::message(message)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CoreValueFactory, LazyValue, ListThunk, PromisedValue};
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    fn access_value(
        context: &EvalContext,
        path: impl Into<Arc<[CoreDataKey]>>,
        arguments: Vec<Value>,
    ) -> Value {
        Value::Lazy(LazyValue::from_access(
            context.values(),
            path.into(),
            Arc::from(arguments),
        ))
    }

    #[test]
    fn scalar_computed_key_resumes_from_its_exact_promise() {
        let context = context();
        let promise = PromisedValue::new(context.values(), "computed access key");
        let base = Value::Dict(
            Dict::new_sync().insert(Key::Number(42.into()), Value::binary_from_text("found")),
        );
        let access = access_value(
            &context,
            [CoreDataKey::Index],
            vec![base, Value::Promised(promise.clone())],
        );

        let blocked = crate::eval::eval_value(&context, &access)
            .expect_err("the dynamic key must wait on its exact promise");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        crate::core::set_test_promise(context.values(), &promise, Value::Number(42.into()))
            .expect("the dynamic key promise should accept its assignment");

        assert_eq!(
            crate::eval::eval_value(&context, &access).expect("computed access should resume"),
            Value::binary_from_text("found")
        );
    }

    #[test]
    fn recursive_dictionary_key_conversion_resumes_at_its_member() {
        let context = context();
        let member = Key::atom_from_text("member");
        let promise = PromisedValue::new(context.values(), "dictionary key member");
        let expected_key = Key::Dict(Arc::from([(member.clone(), Key::Number(7.into()))]));
        let base = Value::Dict(
            Dict::new_sync().insert(expected_key, Value::binary_from_text("recursive")),
        );
        let dynamic =
            Value::Dict(Dict::new_sync().insert(member, Value::Promised(promise.clone())));
        let access = access_value(&context, [CoreDataKey::Index], vec![base, dynamic]);

        crate::eval::eval_value(&context, &access)
            .expect_err("the recursive key member must remain a dependency");
        crate::core::set_test_promise(context.values(), &promise, Value::Number(7.into()))
            .expect("the recursive key promise should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &access).expect("recursive key should resume"),
            Value::binary_from_text("recursive")
        );
    }

    #[test]
    fn computed_path_resumes_after_a_deferred_middle_chunk() {
        let context = context();
        let promise = PromisedValue::new(context.values(), "computed path chunk");
        let base = Value::Dict(Dict::new_sync().insert(
            Key::Number(1.into()),
            Value::Dict(
                Dict::new_sync().insert(Key::Number(2.into()), Value::binary_from_text("path")),
            ),
        ));
        let path = Value::List(List::concat(
            List::from_values(vec![Value::Number(1.into())]),
            List::from_thunk(ListThunk::Promised(promise.clone())),
        ));
        let access = access_value(&context, [CoreDataKey::PathIndex], vec![base, path]);

        crate::eval::eval_value(&context, &access)
            .expect_err("the computed path must wait at its deferred chunk");
        crate::core::set_test_promise(
            context.values(),
            &promise,
            Value::List(List::from_values(vec![Value::Number(2.into())])),
        )
        .expect("the path chunk promise should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &access).expect("computed path should resume"),
            Value::binary_from_text("path")
        );
    }

    #[test]
    fn computed_path_source_rejects_binary_but_deferred_chunks_accept_it() {
        let context = context();
        let base = Value::Dict(Dict::new_sync());
        let invalid = access_value(
            &context,
            [CoreDataKey::PathIndex],
            vec![base.clone(), Value::binary_from_text("x")],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &invalid)
                .expect_err("a path operand itself must remain a list")
                .to_string(),
            "path-list operand must evaluate to a list value"
        );

        let key = Key::Number(Number::from_u8(b'x'));
        let base = Value::Dict(Dict::new_sync().insert(key, Value::binary_from_text("byte")));
        let chunk =
            LazyValue::semantic_thunk(context.values(), "deferred binary path chunk", |_| {
                Ok(Value::binary_from_text("x"))
            });
        let path = Value::List(List::from_thunk(ListThunk::Lazy(chunk)));
        let access = access_value(&context, [CoreDataKey::PathIndex], vec![base, path]);
        assert_eq!(
            crate::eval::eval_value(&context, &access)
                .expect("a deferred binary chunk remains a logical list segment"),
            Value::binary_from_text("byte")
        );
    }
}
