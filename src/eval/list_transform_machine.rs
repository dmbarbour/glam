//! Non-forcing structural list transformations.

use std::sync::Arc;

use crate::core::{Builtin, EvaluatedValue, EvaluationFailure, LazyValue, List, ListThunk, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::number::Number;
use crate::runtime::RuntimeValueRoot;

use super::builtin_machine::BuiltinTaskPoll;
use super::whnf::WhnfComputation;

pub(crate) struct ListMapMachine {
    function: RuntimeValueRoot,
    source: WhnfComputation,
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
