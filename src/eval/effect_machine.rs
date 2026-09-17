//! Durable effect dispatch and fixpoint construction.

use std::sync::Arc;

use crate::core::{Builtin, EvaluationFailure, FixpointComputation, Key, LazyValue, Value};
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
}

impl EffectBuiltinMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::EffectApply | Builtin::EffectCall | Builtin::Fixpoint
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

fn root_message(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<Arc<str>>,
) -> RuntimeFailureRoot {
    context.root_failure(Arc::new(EvaluationFailure::message(message.into())))
}
