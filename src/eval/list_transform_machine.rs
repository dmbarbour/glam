//! Non-forcing structural list transformations.

use std::sync::Arc;

use bytes::Bytes;

use crate::core::{
    Builtin, EvaluatedValue, EvaluationFailure, EvaluationHalt, LazyValue, List, ListThunk, Value,
};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::number::Number;
use crate::runtime::RuntimeValueRoot;

use super::builtin_machine::BuiltinTaskPoll;
use super::list_machine::{ListFrontMachine, ListFrontPoll};
use super::sequence::append_sequence;
use super::value::evaluation_context_frame_in;
use super::whnf::WhnfComputation;

pub(crate) struct ListMapMachine {
    function: RuntimeValueRoot,
    source: WhnfComputation,
}

pub(crate) struct ListConcatMachine {
    source: WhnfComputation,
}

pub(crate) struct TextLinesMachine {
    source: WhnfComputation,
    source_ready: bool,
    front: Option<ListFrontMachine>,
    item: Option<WhnfComputation>,
    bytes: Vec<u8>,
}

impl TextLinesMachine {
    pub(crate) fn new(arguments: Vec<RuntimeValueRoot>) -> Self {
        let [source]: [RuntimeValueRoot; 1] =
            arguments.try_into().expect("text lines retains one source");
        Self {
            source: WhnfComputation::from_root(source),
            source_ready: false,
            front: None,
            item: None,
            bytes: Vec::new(),
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if let Some(item) = &mut self.item {
            let item = match poll_whnf_computation(item, poll_context, durable_context, step_budget)
            {
                WhnfOwnerPoll::Ready(item) => item,
                WhnfOwnerPoll::Pending(dependency) => {
                    return BuiltinTaskPoll::Pending(dependency);
                }
                WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                WhnfOwnerPoll::Failed(failure) => {
                    return BuiltinTaskPoll::Failed(contextual_binary_failure(context, failure));
                }
                WhnfOwnerPoll::External(boundary) => {
                    unreachable!("text-lines item produced an external {boundary:?} boundary")
                }
            };
            let item = context.with_value_access(|access| access.clone_root(&item));
            let item = EvaluatedValue::try_from(item)
                .expect("text-lines item demand must reach WHNF")
                .into_value();
            let byte = match item {
                Value::Number(number) => match number.to_u8_if_integer() {
                    Some(byte) => byte,
                    None => {
                        return failure(
                            context,
                            format!("text lines builtin cannot encode number `{number}` as a byte"),
                        );
                    }
                },
                other => {
                    return failure(
                        context,
                        format!(
                            "text lines builtin requires list items to be byte integers, got {other:?}"
                        ),
                    );
                }
            };
            self.bytes.push(byte);
            self.item = None;
            return BuiltinTaskPoll::Yielded;
        }

        if let Some(front) = &mut self.front {
            return match front.poll(poll_context, context, durable_context, step_budget) {
                ListFrontPoll::Ready(Some((item, tail))) => {
                    self.front = Some(ListFrontMachine::unowned(tail));
                    self.item = Some(WhnfComputation::from_root(item));
                    BuiltinTaskPoll::Yielded
                }
                ListFrontPoll::Ready(None) => {
                    finish_text_lines(context, Bytes::from(std::mem::take(&mut self.bytes)))
                }
                ListFrontPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ListFrontPoll::Yielded => BuiltinTaskPoll::Yielded,
                ListFrontPoll::Failed(failure) => {
                    BuiltinTaskPoll::Failed(contextual_binary_failure(context, failure))
                }
            };
        }

        assert!(
            !self.source_ready,
            "finished text-lines work cannot be polled"
        );
        let source = match poll_whnf_computation(
            &mut self.source,
            poll_context,
            durable_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(source) => source,
            WhnfOwnerPoll::Pending(dependency) => {
                return BuiltinTaskPoll::Pending(dependency);
            }
            WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("text-lines source produced an external {boundary:?} boundary")
            }
        };
        self.source_ready = true;
        match context.with_value_access(|access| access.clone_root(&source)) {
            Value::Binary(bytes) => finish_text_lines(context, bytes),
            Value::List(_) => {
                self.front = Some(ListFrontMachine::unowned(source));
                BuiltinTaskPoll::Yielded
            }
            _ => failure(
                context,
                "text lines builtin requires a binary-compatible list or binary value",
            ),
        }
    }
}

impl ListConcatMachine {
    pub(crate) fn new(arguments: Vec<RuntimeValueRoot>) -> Self {
        let [source]: [RuntimeValueRoot; 1] = arguments
            .try_into()
            .expect("list concat retains one list source");
        Self {
            source: WhnfComputation::from_root(source),
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        let source = match poll_whnf_computation(
            &mut self.source,
            poll_context,
            durable_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(value) => value,
            WhnfOwnerPoll::Pending(dependency) => {
                return BuiltinTaskPoll::Pending(dependency);
            }
            WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("list-concat source produced an external {boundary:?} boundary")
            }
        };

        let result = context.with_value_access(|access| {
            let source = EvaluatedValue::try_from(access.clone_root(&source))
                .expect("list-concat source demand must reach WHNF")
                .into_value();
            let Value::List(source) = source else {
                return Err(EvaluationHalt::new(
                    "list concat builtin requires a list of lists",
                ));
            };
            let flattened = source.flat_map_root_step(
                &mut |_| Ok(invalid_concat_item_in(access.values())),
                &mut |value| Ok(concat_item_in(access.values(), value)),
                &mut |list| deferred_concat_in(access.values(), Value::List(list)),
                &mut |thunk| {
                    deferred_concat_in(
                        access.values(),
                        thunk.duplicate_as_value_in(access.values()),
                    )
                },
            )?;
            Ok(access.values().root_runtime_value(Value::List(flattened)))
        });
        match result {
            Ok(value) => BuiltinTaskPoll::Ready(value),
            Err(error) => {
                BuiltinTaskPoll::Failed(context.root_failure(error.into_permanent_failure()))
            }
        }
    }
}

impl ListMapMachine {
    pub(crate) fn new(arguments: Vec<RuntimeValueRoot>) -> Self {
        let [function, source]: [RuntimeValueRoot; 2] = arguments
            .try_into()
            .expect("map retains a callable and a list source");
        Self {
            function,
            source: WhnfComputation::from_root(source),
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        let source = match poll_whnf_computation(
            &mut self.source,
            poll_context,
            durable_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(value) => value,
            WhnfOwnerPoll::Pending(dependency) => {
                return BuiltinTaskPoll::Pending(dependency);
            }
            WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("map source produced an external {boundary:?} boundary")
            }
        };

        let result = context.with_value_access(|access| {
            let function = access.clone_root(&self.function);
            let source = EvaluatedValue::try_from(access.clone_root(&source))
                .expect("map source demand must reach WHNF")
                .into_value();
            let source = match source {
                Value::Binary(bytes) => List::from_bytes(bytes),
                Value::List(list) => list,
                _ => return None,
            };
            let mapped = source.map_root_step(
                &mut |byte| {
                    lazy_item_in(
                        access.values(),
                        &function,
                        Value::Number(Number::from_u8(byte)),
                    )
                },
                &mut |value| {
                    lazy_item_in(
                        access.values(),
                        &function,
                        access.values().duplicate_value(value),
                    )
                },
                &mut |list| deferred_map_in(access.values(), &function, Value::List(list)),
                &mut |thunk| {
                    deferred_map_in(
                        access.values(),
                        &function,
                        thunk.duplicate_as_value_in(access.values()),
                    )
                },
            );
            Some(access.values().root_runtime_value(Value::List(mapped)))
        });
        match result {
            Some(value) => BuiltinTaskPoll::Ready(value),
            None => BuiltinTaskPoll::Failed(context.root_failure(Arc::new(
                EvaluationFailure::message("map builtin requires a list or binary value"),
            ))),
        }
    }
}

fn lazy_item_in(
    access: &crate::core::RuntimeValueAccess<'_>,
    function: &Value,
    item: Value,
) -> Value {
    Value::Lazy(LazyValue::from_application_in(
        access,
        access.duplicate_value(function),
        Arc::from([item]),
    ))
}

fn deferred_map_in(
    access: &crate::core::RuntimeValueAccess<'_>,
    function: &Value,
    source: Value,
) -> ListThunk {
    let Value::Lazy(mapped) = Value::builtin_call_in(
        access,
        Builtin::Map,
        vec![access.duplicate_value(function), source],
    ) else {
        unreachable!("a saturated map call must remain lazy")
    };
    ListThunk::Lazy(mapped)
}

fn deferred_concat_in(access: &crate::core::RuntimeValueAccess<'_>, source: Value) -> ListThunk {
    let Value::Lazy(flattened) = Value::builtin_call_in(access, Builtin::ListConcat, vec![source])
    else {
        unreachable!("a saturated list-concat call must remain lazy")
    };
    ListThunk::Lazy(flattened)
}

fn concat_item_in(access: &crate::core::RuntimeValueAccess<'_>, item: &Value) -> List {
    append_sequence(access, access.duplicate_value(item))
        .unwrap_or_else(|_| invalid_concat_item_in(access))
}

fn invalid_concat_item_in(access: &crate::core::RuntimeValueAccess<'_>) -> List {
    List::from_thunk(
        LazyValue::error_in(
            access,
            "append requires list or binary values on both sides",
        )
        .into(),
    )
}

fn finish_text_lines(context: &EvaluatorStepContext<'_>, bytes: Bytes) -> BuiltinTaskPoll {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(Value::Binary(bytes.slice(start..index)));
            start = index + 1;
        }
    }
    lines.push(Value::Binary(bytes.slice(start..bytes.len())));
    BuiltinTaskPoll::Ready(context.with_value_access(|access| {
        access
            .values()
            .root_runtime_value(Value::List(List::from_values(lines)))
    }))
}

fn contextual_binary_failure(
    context: &EvaluatorStepContext<'_>,
    failure: crate::runtime::RuntimeFailureRoot,
) -> crate::runtime::RuntimeFailureRoot {
    let failure = context.with_value_access(|access| {
        EvaluationHalt::failure(failure.into_failure()).with_context(
            access.values(),
            evaluation_context_frame_in(access.values(), "binary_extraction"),
        )
    });
    context.root_failure(failure.into_permanent_failure())
}

fn failure(context: &EvaluatorStepContext<'_>, message: impl Into<String>) -> BuiltinTaskPoll {
    let message = message.into();
    BuiltinTaskPoll::Failed(context.root_failure(Arc::new(EvaluationFailure::message(message))))
}
