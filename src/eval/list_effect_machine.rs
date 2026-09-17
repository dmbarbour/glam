//! Pollable source owner for the closed lazy list-effect recipe family.

use std::sync::Arc;

use crate::core::{
    Builtin, Dict, EvaluationFailure, EvaluationHalt, LazyId, LazyValue, List,
    ListEffectComputation, ManagedPromiseRoot, RuntimeValueAccess, Value, keys,
};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::list_machine::{ListFrontMachine, ListFrontPoll};
use super::value::is_undefined_dict_value;
use super::whnf::WhnfComputation;

pub(super) enum ListEffectSourcePoll {
    Ready(RuntimeValueRoot),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(super) struct ListEffectSourceMachine {
    source_owner: LazyId,
    state: ListEffectState,
}

enum ListEffectState {
    Run {
        phase: RunPhase,
        demand: WhnfComputation,
    },
    Sequence {
        continuation: RuntimeValueRoot,
        front: ListFrontMachine,
    },
    Cut {
        front: ListFrontMachine,
    },
    Fix {
        handle: ManagedPromiseRoot,
        front: ListFrontMachine,
    },
}

enum RunPhase {
    Effect,
    Application,
}

impl ListEffectSourceMachine {
    pub(super) fn new(
        context: &EvaluatorStepContext<'_>,
        source_owner: LazyId,
        recipe: &ListEffectComputation,
    ) -> Self {
        let state = match recipe {
            ListEffectComputation::Run { effect } => {
                let effect = context.with_value_access(|access| {
                    access
                        .values()
                        .root_runtime_value(access.values().duplicate_value(effect))
                });
                ListEffectState::Run {
                    phase: RunPhase::Effect,
                    demand: WhnfComputation::from_root(effect).with_source_owner(source_owner),
                }
            }
            ListEffectComputation::Sequence {
                results,
                continuation,
            } => {
                let (results, continuation) = context.with_value_access(|access| {
                    (
                        access
                            .values()
                            .root_runtime_value(Value::List(results.clone())),
                        access
                            .values()
                            .root_runtime_value(access.values().duplicate_value(continuation)),
                    )
                });
                ListEffectState::Sequence {
                    continuation,
                    front: ListFrontMachine::new(results, source_owner),
                }
            }
            ListEffectComputation::Cut { operation } => {
                let operation = context.with_value_access(|access| {
                    access
                        .values()
                        .root_runtime_value(access.values().duplicate_value(operation))
                });
                let results = deferred_run_list_root(context, &operation);
                ListEffectState::Cut {
                    front: ListFrontMachine::new(results, source_owner),
                }
            }
            ListEffectComputation::Fix { operation, handle } => {
                let (operation, handle) = context.with_value_access(|access| {
                    let Value::Promised(handle) = handle else {
                        unreachable!("list-effect fix recipe must retain its promise handle")
                    };
                    (
                        access
                            .values()
                            .root_runtime_value(access.values().duplicate_value(operation)),
                        handle.root_in(access.values()),
                    )
                });
                let results = deferred_run_list_root(context, &operation);
                ListEffectState::Fix {
                    handle,
                    front: ListFrontMachine::new(results, source_owner),
                }
            }
        };
        Self {
            source_owner,
            state,
        }
    }

    pub(super) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ListEffectSourcePoll {
        match &mut self.state {
            ListEffectState::Run { phase, demand } => {
                let value =
                    match poll_whnf_computation(demand, poll_context, durable_context, step_budget)
                    {
                        WhnfOwnerPoll::Ready(value) => value,
                        WhnfOwnerPoll::Pending(dependency) => {
                            return ListEffectSourcePoll::Pending(dependency);
                        }
                        WhnfOwnerPoll::Yielded => return ListEffectSourcePoll::Yielded,
                        WhnfOwnerPoll::Failed(failure) => {
                            return ListEffectSourcePoll::Failed(failure);
                        }
                        WhnfOwnerPoll::External(boundary) => {
                            unreachable!("list effect produced an external {boundary:?} boundary")
                        }
                    };
                match phase {
                    RunPhase::Effect => {
                        let function = match effect_function(context, &value) {
                            Ok(function) => function,
                            Err(error) => {
                                return ListEffectSourcePoll::Failed(root_halt(context, error));
                            }
                        };
                        let api = context.with_value_access(|access| {
                            access
                                .values()
                                .root_runtime_value(list_effect_api(access.values()))
                        });
                        *demand = application_in(context, function, api, self.source_owner);
                        *phase = RunPhase::Application;
                        ListEffectSourcePoll::Yielded
                    }
                    RunPhase::Application => {
                        if context.with_value_access(|access| {
                            matches!(access.clone_root(&value), Value::List(_))
                        }) {
                            ListEffectSourcePoll::Ready(value)
                        } else {
                            let message = context.with_value_access(|access| {
                                let value = access.clone_root(&value);
                                format!(
                                    "list effect handler expected a standard effect result list, got {value:?}"
                                )
                            });
                            ListEffectSourcePoll::Failed(root_message(context, message))
                        }
                    }
                }
            }
            ListEffectState::Sequence {
                continuation,
                front,
            } => match front.poll(poll_context, context, durable_context, step_budget) {
                ListFrontPoll::Ready(None) => {
                    ListEffectSourcePoll::Ready(context.root_value(Value::List(List::empty())))
                }
                ListFrontPoll::Ready(Some((head, tail))) => ListEffectSourcePoll::Ready(
                    sequence_result(context, continuation, &head, &tail),
                ),
                ListFrontPoll::Pending(dependency) => ListEffectSourcePoll::Pending(dependency),
                ListFrontPoll::Yielded => ListEffectSourcePoll::Yielded,
                ListFrontPoll::Failed(failure) => ListEffectSourcePoll::Failed(failure),
            },
            ListEffectState::Cut { front } => {
                match front.poll(poll_context, context, durable_context, step_budget) {
                    ListFrontPoll::Ready(None) => {
                        ListEffectSourcePoll::Ready(context.root_value(Value::List(List::empty())))
                    }
                    ListFrontPoll::Ready(Some((head, _))) => {
                        ListEffectSourcePoll::Ready(context.with_value_access(|access| {
                            access
                                .values()
                                .root_runtime_value(Value::List(List::from_values(vec![
                                    access.clone_root(&head),
                                ])))
                        }))
                    }
                    ListFrontPoll::Pending(dependency) => ListEffectSourcePoll::Pending(dependency),
                    ListFrontPoll::Yielded => ListEffectSourcePoll::Yielded,
                    ListFrontPoll::Failed(failure) => ListEffectSourcePoll::Failed(failure),
                }
            }
            ListEffectState::Fix { handle, front } => {
                match front.poll(poll_context, context, durable_context, step_budget) {
                    ListFrontPoll::Ready(result) => {
                        match publish_fix_result(context, handle, result) {
                            Ok(value) => ListEffectSourcePoll::Ready(value),
                            Err(error) => ListEffectSourcePoll::Failed(root_halt(context, error)),
                        }
                    }
                    ListFrontPoll::Pending(dependency) => ListEffectSourcePoll::Pending(dependency),
                    ListFrontPoll::Yielded => ListEffectSourcePoll::Yielded,
                    ListFrontPoll::Failed(failure) => ListEffectSourcePoll::Failed(failure),
                }
            }
        }
    }
}

fn effect_function(
    context: &EvaluatorStepContext<'_>,
    effect: &RuntimeValueRoot,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    context.with_value_access(|access| {
        let Value::Dict(dict) = access.clone_root(effect) else {
            let effect = access.clone_root(effect);
            return Err(EvaluationHalt::new(format!(
                "list effect handler requires an effect dictionary, got {effect:?}"
            )));
        };
        let Some(function) = dict.get(&*keys::EFF) else {
            return Err(EvaluationHalt::new(
                "list effect handler requires an `eff` member",
            ));
        };
        if is_undefined_dict_value(access.values(), function) {
            return Err(EvaluationHalt::new(
                "list effect handler requires an `eff` member",
            ));
        }
        Ok(access
            .values()
            .root_runtime_value(access.values().duplicate_value(function)))
    })
}

fn application_in(
    context: &EvaluatorStepContext<'_>,
    function: RuntimeValueRoot,
    argument: RuntimeValueRoot,
    source_owner: LazyId,
) -> WhnfComputation {
    context.with_value_access(|access| {
        let function = access.clone_root(&function);
        let argument = access.clone_root(&argument);
        WhnfComputation::from_application_checkpoint_in(&access, function, &[argument])
            .with_source_owner(source_owner)
    })
}

fn deferred_run_list_root(
    context: &EvaluatorStepContext<'_>,
    operation: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    let lazy = context.construct_lazy(|access| {
        LazyValue::list_effect_computation_in(
            access,
            "list effect",
            ListEffectComputation::Run {
                effect: operation.clone_core_with(access),
            },
        )
    });
    context.root_value(Value::List(List::from_thunk(lazy.into())))
}

fn sequence_result(
    context: &EvaluatorStepContext<'_>,
    continuation: &RuntimeValueRoot,
    head: &RuntimeValueRoot,
    tail: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    let left = context.construct_lazy(|access| {
        let application = LazyValue::from_application_in(
            access,
            continuation.clone_core_with(access),
            Arc::from([head.clone_core_with(access)]),
        );
        LazyValue::list_effect_computation_in(
            access,
            "list effect",
            ListEffectComputation::Run {
                effect: Value::Lazy(application),
            },
        )
    });
    let right = context.construct_lazy(|access| {
        let Value::List(tail) = tail.clone_core_with(access) else {
            unreachable!("list-effect sequence must retain a list tail")
        };
        LazyValue::list_effect_computation_in(
            access,
            "list effect seq",
            ListEffectComputation::Sequence {
                results: tail,
                continuation: continuation.clone_core_with(access),
            },
        )
    });
    context.root_value(Value::List(List::concat(
        List::from_thunk(left.into()),
        List::from_thunk(right.into()),
    )))
}

fn publish_fix_result(
    context: &EvaluatorStepContext<'_>,
    handle: &ManagedPromiseRoot,
    result: Option<(RuntimeValueRoot, RuntimeValueRoot)>,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    let (published, value) = context.with_value_access(|access| match result {
        None => {
            let empty = Value::List(List::empty());
            let published = handle.publish(access.values(), Ok(empty.clone()));
            (published, access.values().root_runtime_value(empty))
        }
        Some((head, tail)) => {
            let head = access.clone_root(&head);
            let Value::List(tail) = access.clone_root(&tail) else {
                unreachable!("list-effect fix must retain a list tail")
            };
            let published = handle.publish(access.values(), Ok(head.clone()));
            let value = Value::List(List::concat(List::from_values(vec![head]), tail));
            (published, access.values().root_runtime_value(value))
        }
    });
    let published =
        published.map_err(|_| EvaluationHalt::new("list effect fix initialized twice"))?;
    published.notify();
    Ok(value)
}

fn list_effect_api(_access: &RuntimeValueAccess<'_>) -> Value {
    Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::R).clone(),
                Value::Builtin(Builtin::ListEffectReturn),
            )
            .insert((*keys::SEQ).clone(), Value::Builtin(Builtin::ListEffectSeq))
            .insert((*keys::ALT).clone(), Value::Builtin(Builtin::ListEffectAlt))
            .insert((*keys::FAIL).clone(), Value::List(List::empty()))
            .insert((*keys::CUT).clone(), Value::Builtin(Builtin::ListEffectCut))
            .insert((*keys::FIX).clone(), Value::Builtin(Builtin::ListEffectFix)),
    )
}

fn root_halt(context: &EvaluatorStepContext<'_>, halt: EvaluationHalt) -> RuntimeFailureRoot {
    context.root_failure(halt.into_permanent_failure())
}

fn root_message(
    context: &EvaluatorStepContext<'_>,
    message: impl AsRef<str>,
) -> RuntimeFailureRoot {
    context.root_failure(Arc::new(EvaluationFailure::message(message)))
}
