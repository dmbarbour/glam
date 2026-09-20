//! Non-forcing structural list transformations.

use std::sync::Arc;

use bytes::Bytes;
use glam_gc::Visitor;

use crate::core::{
    Builtin, EvaluatedValue, EvaluationFailure, EvaluationHalt, LazyId, LazyValue, List, ListThunk,
    Value, trace_compatibility_value_managed_edges,
};
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};
use crate::number::Number;

use super::builtin_machine::RegionalBuiltinPoll;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::sequence::append_sequence;
use super::value::evaluation_context_frame_in;
use super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place,
    reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalListMapMachine {
    function: Value,
    source: Option<Value>,
    source_demand: Option<RegionalWhnfWork>,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalListConcatMachine {
    source: Option<Value>,
    source_demand: Option<RegionalWhnfWork>,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalTextLinesMachine {
    source: Option<Value>,
    source_demand: Option<RegionalWhnfWork>,
    front: Option<RegionalListFront>,
    item: Option<RegionalWhnfWork>,
    bytes: Vec<u8>,
    source_owner: LazyId,
}

impl RegionalTextLinesMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let [source] = arguments else {
            panic!("text lines retains one source")
        };
        Self {
            source: Some(access.values().duplicate_value(source)),
            source_demand: None,
            front: None,
            item: None,
            bytes: Vec::new(),
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if let Some(item) = &mut self.item {
            let item =
                match drive_regional_in_place(access, item, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(item) => item,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(contextual_binary_failure_in(
                            access, failure,
                        ));
                    }
                };
            let item = EvaluatedValue::try_from(item)
                .expect("text-lines item demand must reach WHNF")
                .into_value();
            let byte = match item {
                Value::Number(number) => match number.to_u8_if_integer() {
                    Some(byte) => byte,
                    None => {
                        return failure_in(format!(
                            "text lines builtin cannot encode number `{number}` as a byte"
                        ));
                    }
                },
                other => {
                    return failure_in(format!(
                        "text lines builtin requires list items to be byte integers, got {other:?}"
                    ));
                }
            };
            self.bytes.push(byte);
            self.item = None;
            return RegionalBuiltinPoll::Yielded;
        }

        if let Some(front) = &mut self.front {
            return match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(Some((item, tail))) => {
                    self.front = Some(RegionalListFront::new_in(
                        access,
                        tail,
                        Some(self.source_owner),
                    ));
                    self.item = Some(
                        RegionalWhnfWork::from_focus(access, item)
                            .with_source_owner(self.source_owner),
                    );
                    RegionalBuiltinPoll::Yielded
                }
                RegionalListFrontPoll::Ready(None) => RegionalBuiltinPoll::Ready(
                    finish_text_lines_in(access, Bytes::from(std::mem::take(&mut self.bytes))),
                ),
                RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalListFrontPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => {
                    RegionalBuiltinPoll::Failed(contextual_binary_failure_in(access, failure))
                }
            };
        }

        let source = match poll_regional_source(
            access,
            &mut self.source,
            &mut self.source_demand,
            self.source_owner,
            step_budget,
        ) {
            RegionalSourcePoll::Ready(source) => source,
            RegionalSourcePoll::Boundary(request) => {
                return RegionalBuiltinPoll::Boundary(request);
            }
            RegionalSourcePoll::Yielded => return RegionalBuiltinPoll::Yielded,
            RegionalSourcePoll::Failed(failure) => return RegionalBuiltinPoll::Failed(failure),
        };
        match source {
            Value::Binary(bytes) => RegionalBuiltinPoll::Ready(finish_text_lines_in(access, bytes)),
            source @ Value::List(_) => {
                self.front = Some(RegionalListFront::new_in(
                    access,
                    source,
                    Some(self.source_owner),
                ));
                RegionalBuiltinPoll::Yielded
            }
            _ => failure_in("text lines builtin requires a binary-compatible list or binary value"),
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(source) = &self.source {
            trace_compatibility_value_managed_edges(source, visitor);
        }
        if let Some(demand) = &self.source_demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(front) = &self.front {
            front.trace_managed_edges(visitor);
        }
        if let Some(item) = &self.item {
            item.trace_managed_edges(visitor);
        }
    }
}

impl RegionalListConcatMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let [source] = arguments else {
            panic!("list concat retains one list source")
        };
        Self {
            source: Some(access.values().duplicate_value(source)),
            source_demand: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        let source = match poll_regional_source(
            access,
            &mut self.source,
            &mut self.source_demand,
            self.source_owner,
            step_budget,
        ) {
            RegionalSourcePoll::Ready(source) => source,
            RegionalSourcePoll::Boundary(request) => {
                return RegionalBuiltinPoll::Boundary(request);
            }
            RegionalSourcePoll::Yielded => return RegionalBuiltinPoll::Yielded,
            RegionalSourcePoll::Failed(failure) => return RegionalBuiltinPoll::Failed(failure),
        };
        let Value::List(source) = source else {
            return failure_in("list concat builtin requires a list of lists");
        };
        let flattened = source
            .flat_map_root_step(
                &mut |_| Ok::<_, EvaluationHalt>(invalid_concat_item_in(access.values())),
                &mut |value| Ok::<_, EvaluationHalt>(concat_item_in(access.values(), value)),
                &mut |list| deferred_concat_in(access.values(), Value::List(list)),
                &mut |thunk| {
                    deferred_concat_in(
                        access.values(),
                        thunk.duplicate_as_value_in(access.values()),
                    )
                },
            )
            .expect("list-concat structural leaf conversion is infallible");
        RegionalBuiltinPoll::Ready(Value::List(flattened))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(source) = &self.source {
            trace_compatibility_value_managed_edges(source, visitor);
        }
        if let Some(demand) = &self.source_demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

impl RegionalListMapMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let [function, source] = arguments else {
            panic!("map retains a callable and a list source")
        };
        Self {
            function: access.values().duplicate_value(function),
            source: Some(access.values().duplicate_value(source)),
            source_demand: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        let source = match poll_regional_source(
            access,
            &mut self.source,
            &mut self.source_demand,
            self.source_owner,
            step_budget,
        ) {
            RegionalSourcePoll::Ready(source) => source,
            RegionalSourcePoll::Boundary(request) => {
                return RegionalBuiltinPoll::Boundary(request);
            }
            RegionalSourcePoll::Yielded => return RegionalBuiltinPoll::Yielded,
            RegionalSourcePoll::Failed(failure) => return RegionalBuiltinPoll::Failed(failure),
        };
        let source = match source {
            Value::Binary(bytes) => List::from_bytes(bytes),
            Value::List(list) => list,
            _ => return failure_in("map builtin requires a list or binary value"),
        };
        let mapped = source.map_root_step(
            &mut |byte| {
                lazy_item_in(
                    access.values(),
                    &self.function,
                    Value::Number(Number::from_u8(byte)),
                )
            },
            &mut |value| {
                lazy_item_in(
                    access.values(),
                    &self.function,
                    access.values().duplicate_value(value),
                )
            },
            &mut |list| deferred_map_in(access.values(), &self.function, Value::List(list)),
            &mut |thunk| {
                deferred_map_in(
                    access.values(),
                    &self.function,
                    thunk.duplicate_as_value_in(access.values()),
                )
            },
        );
        RegionalBuiltinPoll::Ready(Value::List(mapped))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.function, visitor);
        if let Some(source) = &self.source {
            trace_compatibility_value_managed_edges(source, visitor);
        }
        if let Some(demand) = &self.source_demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

enum RegionalSourcePoll {
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

fn poll_regional_source(
    access: &EvaluationValueAccess<'_>,
    source: &mut Option<Value>,
    demand: &mut Option<RegionalWhnfWork>,
    source_owner: LazyId,
    step_budget: &mut EvaluationStepBudget,
) -> RegionalSourcePoll {
    if demand.is_none() {
        let source = source
            .take()
            .expect("regional list transform must retain its source");
        *demand =
            Some(RegionalWhnfWork::from_focus(access, source).with_source_owner(source_owner));
    }
    match drive_regional_in_place(
        access,
        demand
            .as_mut()
            .expect("regional list transform source demand must be installed"),
        step_budget,
        reduce_semantic_shell,
    ) {
        RegionalWhnfStatus::Ready(source) => {
            *demand = None;
            RegionalSourcePoll::Ready(
                EvaluatedValue::try_from(source)
                    .expect("regional list-transform source demand must reach WHNF")
                    .into_value(),
            )
        }
        RegionalWhnfStatus::Boundary(request) => RegionalSourcePoll::Boundary(request),
        RegionalWhnfStatus::Yielded => RegionalSourcePoll::Yielded,
        RegionalWhnfStatus::Failed(failure) => RegionalSourcePoll::Failed(failure),
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

fn finish_text_lines_in(_access: &EvaluationValueAccess<'_>, bytes: Bytes) -> Value {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(Value::Binary(bytes.slice(start..index)));
            start = index + 1;
        }
    }
    lines.push(Value::Binary(bytes.slice(start..bytes.len())));
    Value::List(List::from_values(lines))
}

fn contextual_binary_failure_in(
    access: &EvaluationValueAccess<'_>,
    failure: Arc<EvaluationFailure>,
) -> Arc<EvaluationFailure> {
    EvaluationHalt::failure(failure)
        .with_context(
            access.values(),
            evaluation_context_frame_in(access.values(), "binary_extraction"),
        )
        .into_permanent_failure()
}

fn failure_in(message: impl Into<String>) -> RegionalBuiltinPoll {
    let message = message.into();
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message)))
}
