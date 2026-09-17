//! Durable effect dispatch and fixpoint construction.

use std::sync::Arc;

use crate::core::{
    Builtin, BuiltinCall, EvaluationFailure, FixpointComputation, Key, LazyValue, List, Value, keys,
};
use crate::core_net::CoreDataKey;
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::access_machine::{ConversionPoll, KeyConversionMachine};
use super::builtin_machine::BuiltinTaskPoll;
use super::list_machine::{ListFrontMachine, ListFrontPoll};
use super::whnf::WhnfComputation;

pub(crate) struct EffectBuiltinMachine {
    phase: EffectPhase,
}

enum EffectPhase {
    Apply {
        function: WhnfComputation,
        argument: RuntimeValueRoot,
        api: RuntimeValueRoot,
    },
    CallName {
        name: KeyConversionMachine,
        arguments: RuntimeValueRoot,
        api: RuntimeValueRoot,
    },
    CallArguments {
        name: Key,
        arguments: WhnfComputation,
        api: RuntimeValueRoot,
    },
    CallItems {
        name: Key,
        items: ListFrontMachine,
        arguments: Vec<RuntimeValueRoot>,
        api: RuntimeValueRoot,
    },
    Fixpoint {
        function: WhnfComputation,
    },
    Map {
        function: RuntimeValueRoot,
        items: RuntimeValueRoot,
    },
    MapItems {
        function: RuntimeValueRoot,
        items: WhnfComputation,
        results: RuntimeValueRoot,
        api: RuntimeValueRoot,
    },
    MapResults {
        function: RuntimeValueRoot,
        items: RuntimeValueRoot,
        results: WhnfComputation,
        api: RuntimeValueRoot,
    },
    MapFront {
        function: RuntimeValueRoot,
        items: ListFrontMachine,
        results: RuntimeValueRoot,
        api: RuntimeValueRoot,
    },
    MapContinue {
        function: RuntimeValueRoot,
        items: RuntimeValueRoot,
        results: RuntimeValueRoot,
        result: RuntimeValueRoot,
    },
}

impl EffectBuiltinMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::EffectApply
                | Builtin::EffectCall
                | Builtin::Fixpoint
                | Builtin::EffectMap
                | Builtin::EffectMapRun
                | Builtin::EffectMapContinue
        )
    }

    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let phase = match builtin {
            Builtin::EffectApply => {
                let [function, argument, api]: [RuntimeValueRoot; 3] = arguments
                    .try_into()
                    .expect("effect apply retains three operands");
                EffectPhase::Apply {
                    function: WhnfComputation::from_root(function),
                    argument,
                    api,
                }
            }
            Builtin::EffectCall => {
                let [name, arguments, api]: [RuntimeValueRoot; 3] = arguments
                    .try_into()
                    .expect("effect call retains three operands");
                EffectPhase::CallName {
                    name: KeyConversionMachine::new(name, None),
                    arguments,
                    api,
                }
            }
            Builtin::Fixpoint => {
                let [function]: [RuntimeValueRoot; 1] =
                    arguments.try_into().expect("fixpoint retains one operand");
                EffectPhase::Fixpoint {
                    function: WhnfComputation::from_root(function),
                }
            }
            Builtin::EffectMap => {
                let [function, items]: [RuntimeValueRoot; 2] = arguments
                    .try_into()
                    .expect("effect map retains two operands");
                EffectPhase::Map { function, items }
            }
            Builtin::EffectMapRun => {
                let [function, items, results, api]: [RuntimeValueRoot; 4] = arguments
                    .try_into()
                    .expect("effect map run retains four operands");
                EffectPhase::MapItems {
                    function,
                    items: WhnfComputation::from_root(items),
                    results,
                    api,
                }
            }
            Builtin::EffectMapContinue => {
                let [function, items, results, result]: [RuntimeValueRoot; 4] = arguments
                    .try_into()
                    .expect("effect map continuation retains four operands");
                EffectPhase::MapContinue {
                    function,
                    items,
                    results,
                    result,
                }
            }
            _ => unreachable!("effect machine received another builtin"),
        };
        Self { phase }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        match &mut self.phase {
            EffectPhase::Apply {
                function,
                argument,
                api,
            } => {
                let function = match poll_whnf(function, poll_context, durable_context, step_budget)
                {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                BuiltinTaskPoll::Ready(root_application(context, &function, &[api, argument]))
            }
            EffectPhase::CallName {
                name,
                arguments,
                api,
            } => {
                let name = match name.poll(poll_context, context, durable_context, step_budget) {
                    ConversionPoll::Ready(name) => name,
                    ConversionPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    ConversionPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    ConversionPoll::Failed(failure) => {
                        return BuiltinTaskPoll::Failed(failure);
                    }
                };
                self.phase = EffectPhase::CallArguments {
                    name,
                    arguments: WhnfComputation::from_root(arguments.clone()),
                    api: api.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            EffectPhase::CallArguments {
                name,
                arguments,
                api,
            } => {
                let arguments =
                    match poll_whnf(arguments, poll_context, durable_context, step_budget) {
                        DemandResult::Ready(value) => value,
                        DemandResult::Pending(poll) => return poll,
                    };
                let is_list = context.with_value_access(|access| {
                    matches!(access.clone_root(&arguments), Value::List(_))
                });
                if !is_list {
                    return BuiltinTaskPoll::Failed(root_message(
                        context,
                        "effect call builtin requires a list of arguments",
                    ));
                }
                self.phase = EffectPhase::CallItems {
                    name: name.clone(),
                    items: ListFrontMachine::unowned(arguments),
                    arguments: Vec::new(),
                    api: api.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            EffectPhase::CallItems {
                name,
                items,
                arguments,
                api,
            } => match items.poll(poll_context, context, durable_context, step_budget) {
                ListFrontPoll::Ready(Some((argument, tail))) => {
                    arguments.push(argument);
                    *items = ListFrontMachine::unowned(tail);
                    BuiltinTaskPoll::Yielded
                }
                ListFrontPoll::Ready(None) => {
                    BuiltinTaskPoll::Ready(root_effect_call(context, api, name, arguments))
                }
                ListFrontPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ListFrontPoll::Yielded => BuiltinTaskPoll::Yielded,
                ListFrontPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            },
            EffectPhase::Fixpoint { function } => {
                let function = match poll_whnf(function, poll_context, durable_context, step_budget)
                {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let valid = context.with_value_access(|access| {
                    matches!(
                        access.clone_root(&function),
                        Value::Function(_) | Value::Net(_)
                    )
                });
                if !valid {
                    return BuiltinTaskPoll::Failed(root_message(
                        context,
                        "fixpoint builtin requires a function value",
                    ));
                }
                BuiltinTaskPoll::Ready(context.with_value_access(|access| {
                    let function = access.clone_root(&function);
                    let lazy = LazyValue::computed_fixpoint_in(
                        access.values(),
                        "fixpoint",
                        FixpointComputation::Function(function),
                    );
                    access.values().root_runtime_value(Value::Lazy(lazy))
                }))
            }
            EffectPhase::Map { function, items } => {
                BuiltinTaskPoll::Ready(root_effect_map(context, function, items))
            }
            EffectPhase::MapItems {
                function,
                items,
                results,
                api,
            } => {
                let items = match poll_whnf(items, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let is_list = context.with_value_access(|access| {
                    matches!(access.clone_root(&items), Value::List(_))
                });
                if !is_list {
                    return BuiltinTaskPoll::Failed(root_message(
                        context,
                        "effect map requires a list",
                    ));
                }
                self.phase = EffectPhase::MapResults {
                    function: function.clone(),
                    items,
                    results: WhnfComputation::from_root(results.clone()),
                    api: api.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            EffectPhase::MapResults {
                function,
                items,
                results,
                api,
            } => {
                let results = match poll_whnf(results, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let is_list = context.with_value_access(|access| {
                    matches!(access.clone_root(&results), Value::List(_))
                });
                if !is_list {
                    return BuiltinTaskPoll::Failed(root_message(
                        context,
                        "effect map internal results must be a list",
                    ));
                }
                self.phase = EffectPhase::MapFront {
                    function: function.clone(),
                    items: ListFrontMachine::unowned(items.clone()),
                    results,
                    api: api.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            EffectPhase::MapFront {
                function,
                items,
                results,
                api,
            } => match items.poll(poll_context, context, durable_context, step_budget) {
                ListFrontPoll::Ready(Some((item, tail))) => BuiltinTaskPoll::Ready(
                    root_effect_map_sequence(context, function, &item, &tail, results, api),
                ),
                ListFrontPoll::Ready(None) => BuiltinTaskPoll::Ready(root_effect_call(
                    context,
                    api,
                    &keys::R,
                    std::slice::from_ref(results),
                )),
                ListFrontPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ListFrontPoll::Yielded => BuiltinTaskPoll::Yielded,
                ListFrontPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            },
            EffectPhase::MapContinue {
                function,
                items,
                results,
                result,
            } => match root_effect_map_continuation(context, function, items, results, result) {
                Ok(value) => BuiltinTaskPoll::Ready(value),
                Err(failure) => BuiltinTaskPoll::Failed(failure),
            },
        }
    }
}

enum DemandResult {
    Ready(RuntimeValueRoot),
    Pending(BuiltinTaskPoll),
}

fn poll_whnf(
    computation: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    durable_context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> DemandResult {
    match poll_whnf_computation(computation, poll_context, durable_context, step_budget) {
        WhnfOwnerPoll::Ready(value) => DemandResult::Ready(value),
        WhnfOwnerPoll::Pending(dependency) => {
            DemandResult::Pending(BuiltinTaskPoll::Pending(dependency))
        }
        WhnfOwnerPoll::Yielded => DemandResult::Pending(BuiltinTaskPoll::Yielded),
        WhnfOwnerPoll::Failed(failure) => DemandResult::Pending(BuiltinTaskPoll::Failed(failure)),
        WhnfOwnerPoll::External(boundary) => {
            unreachable!("effect builtin produced an external {boundary:?} boundary")
        }
    }
}

fn root_application(
    context: &EvaluatorStepContext<'_>,
    function: &RuntimeValueRoot,
    arguments: &[&RuntimeValueRoot],
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let function = access.clone_root(function);
        let arguments = arguments
            .iter()
            .map(|argument| access.clone_root(argument))
            .collect::<Vec<_>>();
        let lazy = LazyValue::from_application_in(access.values(), function, Arc::from(arguments));
        access.values().root_runtime_value(Value::Lazy(lazy))
    })
}

fn root_effect_call(
    context: &EvaluatorStepContext<'_>,
    api: &RuntimeValueRoot,
    name: &Key,
    arguments: &[RuntimeValueRoot],
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let selected = LazyValue::from_access_in(
            access.values(),
            Arc::from([CoreDataKey::Key(name.clone())]),
            Arc::from([access.clone_root(api)]),
        );
        let operation = if arguments.is_empty() {
            selected
        } else {
            let arguments = arguments
                .iter()
                .map(|argument| access.clone_root(argument))
                .collect::<Vec<_>>();
            LazyValue::from_application_in(
                access.values(),
                Value::Lazy(selected),
                Arc::from(arguments),
            )
        };
        access.values().root_runtime_value(Value::Lazy(operation))
    })
}

fn root_effect_map_sequence(
    context: &EvaluatorStepContext<'_>,
    function: &RuntimeValueRoot,
    item: &RuntimeValueRoot,
    tail: &RuntimeValueRoot,
    results: &RuntimeValueRoot,
    api: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let operation = Value::Lazy(LazyValue::from_application_in(
            access.values(),
            access.clone_root(function),
            Arc::from([access.clone_root(item)]),
        ));
        let continuation = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::EffectMapContinue,
            arguments: Arc::from([
                access.clone_root(function),
                access.clone_root(tail),
                access.clone_root(results),
            ]),
        });
        let selected = LazyValue::from_access_in(
            access.values(),
            Arc::from([CoreDataKey::Key((*keys::SEQ).clone())]),
            Arc::from([access.clone_root(api)]),
        );
        let sequence = LazyValue::from_application_in(
            access.values(),
            Value::Lazy(selected),
            Arc::from([operation, continuation]),
        );
        access.values().root_runtime_value(Value::Lazy(sequence))
    })
}

fn root_effect_map(
    context: &EvaluatorStepContext<'_>,
    function: &RuntimeValueRoot,
    items: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let function = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::EffectMapRun,
            arguments: Arc::from([
                access.clone_root(function),
                access.clone_root(items),
                Value::List(List::empty()),
            ]),
        });
        access
            .values()
            .root_runtime_value(super::application::effect_value(access.values(), function))
    })
}

fn root_effect_map_continuation(
    context: &EvaluatorStepContext<'_>,
    function: &RuntimeValueRoot,
    items: &RuntimeValueRoot,
    results: &RuntimeValueRoot,
    result: &RuntimeValueRoot,
) -> Result<RuntimeValueRoot, RuntimeFailureRoot> {
    context.with_value_access(|access| {
        let Value::List(results) = access.clone_root(results) else {
            return Err(access.values().root_runtime_failure(Arc::new(
                EvaluationFailure::message("effect map internal results must be a list"),
            )));
        };
        let results = List::concat(results, List::from_values(vec![access.clone_root(result)]));
        let function = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::EffectMapRun,
            arguments: Arc::from([
                access.clone_root(function),
                access.clone_root(items),
                Value::List(results),
            ]),
        });
        Ok(access
            .values()
            .root_runtime_value(super::application::effect_value(access.values(), function)))
    })
}

fn root_message(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<Arc<str>>,
) -> RuntimeFailureRoot {
    context.root_failure(Arc::new(EvaluationFailure::message(message.into())))
}
