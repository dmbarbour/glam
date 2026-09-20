//! Regional reducer for the closed lazy list-effect recipe family.
//!
//! Every poll-spanning semantic edge lives beneath the owning lazy's typed
//! managed checkpoint. Boundary interpretation and terminal publication stay
//! with the outer evaluator route.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Builtin, Dict, EvaluationFailure, EvaluationHalt, LazyId, LazyValue, List,
    ListEffectComputation, ManagedPromisePublication, PromisedValue, Value, keys,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::EvaluationValueAccess;

use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::value::is_undefined_dict_value;
use super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place,
    reduce_semantic_shell,
};

pub(in crate::eval) enum RegionalListEffectPoll {
    Ready(Value),
    FixReady {
        handle: PromisedValue,
        result: Option<(Value, Value)>,
    },
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

pub(in crate::eval) struct RegionalListEffect {
    source_owner: LazyId,
    state: ListEffectState,
}

enum ListEffectState {
    Run {
        phase: RunPhase,
        demand: RegionalWhnfWork,
    },
    Sequence {
        continuation: Value,
        front: RegionalListFront,
    },
    FlatMapResults {
        continuation: Value,
        front: RegionalListFront,
    },
    Cut {
        front: RegionalListFront,
    },
    FirstResult {
        front: RegionalListFront,
    },
    FixFunction {
        function: RegionalWhnfWork,
    },
    Fix {
        handle: PromisedValue,
        front: RegionalListFront,
    },
}

enum RunPhase {
    Effect,
    Application,
}

impl RegionalListEffect {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        recipe: &ListEffectComputation,
    ) -> Self {
        let state = match recipe {
            ListEffectComputation::Run { effect } => ListEffectState::Run {
                phase: RunPhase::Effect,
                demand: RegionalWhnfWork::from_focus(
                    access,
                    access.values().duplicate_value(effect),
                )
                .with_source_owner(source_owner),
            },
            ListEffectComputation::Sequence {
                results,
                continuation,
            } => ListEffectState::Sequence {
                continuation: access.values().duplicate_value(continuation),
                front: RegionalListFront::new_in(
                    access,
                    Value::List(results.clone()),
                    Some(source_owner),
                ),
            },
            ListEffectComputation::FlatMapResults {
                results,
                continuation,
            } => ListEffectState::FlatMapResults {
                continuation: access.values().duplicate_value(continuation),
                front: RegionalListFront::new_in(
                    access,
                    Value::List(results.clone()),
                    Some(source_owner),
                ),
            },
            ListEffectComputation::Cut { operation } => {
                let operation = access.values().duplicate_value(operation);
                let results = deferred_run_list_in(access, &operation);
                ListEffectState::Cut {
                    front: RegionalListFront::new_in(access, results, Some(source_owner)),
                }
            }
            ListEffectComputation::FirstResult { results } => ListEffectState::FirstResult {
                front: RegionalListFront::new_in(
                    access,
                    Value::List(results.clone()),
                    Some(source_owner),
                ),
            },
            ListEffectComputation::FixFunction { function } => ListEffectState::FixFunction {
                function: RegionalWhnfWork::from_focus(
                    access,
                    access.values().duplicate_value(function),
                )
                .with_source_owner(source_owner),
            },
        };
        Self {
            source_owner,
            state,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalListEffectPoll {
        match &mut self.state {
            ListEffectState::Run { phase, demand } => {
                let value = match drive_regional_in_place(
                    access,
                    demand,
                    step_budget,
                    reduce_semantic_shell,
                ) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalListEffectPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalListEffectPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalListEffectPoll::Failed(failure);
                    }
                };
                match phase {
                    RunPhase::Effect => {
                        let function = match effect_function_in(access, &value) {
                            Ok(function) => function,
                            Err(error) => {
                                return RegionalListEffectPoll::Failed(
                                    error.into_permanent_failure(),
                                );
                            }
                        };
                        *demand = RegionalWhnfWork::from_application_checkpoint_in(
                            access,
                            function,
                            &[list_effect_api(access)],
                            Some(self.source_owner),
                        );
                        *phase = RunPhase::Application;
                        RegionalListEffectPoll::Yielded
                    }
                    RunPhase::Application => {
                        if matches!(value, Value::List(_)) {
                            RegionalListEffectPoll::Ready(value)
                        } else {
                            RegionalListEffectPoll::Failed(Arc::new(EvaluationFailure::message(
                                format!(
                                    "list effect handler expected a standard effect result list, got {value:?}"
                                ),
                            )))
                        }
                    }
                }
            }
            ListEffectState::Sequence {
                continuation,
                front,
            } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(None) => {
                    RegionalListEffectPoll::Ready(Value::List(List::empty()))
                }
                RegionalListFrontPoll::Ready(Some((head, tail))) => RegionalListEffectPoll::Ready(
                    sequence_result_in(access, continuation, &head, &tail),
                ),
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalListEffectPoll::Boundary(request)
                }
                RegionalListFrontPoll::Yielded => RegionalListEffectPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            },
            ListEffectState::FlatMapResults {
                continuation,
                front,
            } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(None) => {
                    RegionalListEffectPoll::Ready(Value::List(List::empty()))
                }
                RegionalListFrontPoll::Ready(Some((head, tail))) => RegionalListEffectPoll::Ready(
                    flat_map_result_in(access, continuation, &head, &tail),
                ),
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalListEffectPoll::Boundary(request)
                }
                RegionalListFrontPoll::Yielded => RegionalListEffectPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            },
            ListEffectState::Cut { front } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(None) => {
                    RegionalListEffectPoll::Ready(Value::List(List::empty()))
                }
                RegionalListFrontPoll::Ready(Some((head, _))) => {
                    RegionalListEffectPoll::Ready(Value::List(List::from_values(vec![head])))
                }
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalListEffectPoll::Boundary(request)
                }
                RegionalListFrontPoll::Yielded => RegionalListEffectPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            },
            ListEffectState::FirstResult { front } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(None) => {
                    RegionalListEffectPoll::Ready(Value::List(List::empty()))
                }
                RegionalListFrontPoll::Ready(Some((head, _))) => {
                    RegionalListEffectPoll::Ready(Value::List(List::from_values(vec![head])))
                }
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalListEffectPoll::Boundary(request)
                }
                RegionalListFrontPoll::Yielded => RegionalListEffectPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            },
            ListEffectState::FixFunction { function } => {
                let function = match drive_regional_in_place(
                    access,
                    function,
                    step_budget,
                    reduce_semantic_shell,
                ) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalListEffectPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalListEffectPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalListEffectPoll::Failed(failure);
                    }
                };
                let handle = access
                    .values()
                    .construct_managed_promise("list effect fixpoint")
                    .expect("managed promise representation must fit one collector run");
                let operation = LazyValue::from_application_in(
                    access.values(),
                    function,
                    Arc::from([Value::Promised(handle.duplicate_in(access.values()))]),
                );
                let results = deferred_run_list_in(access, &Value::Lazy(operation));
                self.state = ListEffectState::Fix {
                    handle,
                    front: RegionalListFront::new_in(access, results, Some(self.source_owner)),
                };
                RegionalListEffectPoll::Yielded
            }
            ListEffectState::Fix { handle, front } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(result) => RegionalListEffectPoll::FixReady {
                    handle: handle.duplicate_in(access.values()),
                    result,
                },
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalListEffectPoll::Boundary(request)
                }
                RegionalListFrontPoll::Yielded => RegionalListEffectPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            },
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.state.trace_managed_edges(visitor);
    }
}

impl ListEffectState {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match self {
            Self::Run { demand, .. } => demand.trace_managed_edges(visitor),
            Self::Sequence {
                continuation,
                front,
            }
            | Self::FlatMapResults {
                continuation,
                front,
            } => {
                trace_compatibility_value_managed_edges(continuation, visitor);
                front.trace_managed_edges(visitor);
            }
            Self::Cut { front } | Self::FirstResult { front } => front.trace_managed_edges(visitor),
            Self::FixFunction { function } => function.trace_managed_edges(visitor),
            Self::Fix { handle, front } => {
                handle.trace_managed_edge(visitor);
                front.trace_managed_edges(visitor);
            }
        }
    }
}

fn effect_function_in(
    access: &EvaluationValueAccess<'_>,
    effect: &Value,
) -> Result<Value, EvaluationHalt> {
    let Value::Dict(dict) = effect else {
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
    Ok(access.values().duplicate_value(function))
}

fn deferred_run_list_in(access: &EvaluationValueAccess<'_>, operation: &Value) -> Value {
    let lazy = LazyValue::list_effect_computation_in(
        access.values(),
        "list effect",
        ListEffectComputation::Run {
            effect: access.values().duplicate_value(operation),
        },
    );
    Value::List(List::from_thunk(lazy.into()))
}

fn sequence_result_in(
    access: &EvaluationValueAccess<'_>,
    continuation: &Value,
    head: &Value,
    tail: &Value,
) -> Value {
    let application = LazyValue::from_application_in(
        access.values(),
        access.values().duplicate_value(continuation),
        Arc::from([access.values().duplicate_value(head)]),
    );
    let left = LazyValue::list_effect_computation_in(
        access.values(),
        "list effect",
        ListEffectComputation::Run {
            effect: Value::Lazy(application),
        },
    );
    let Value::List(tail) = tail else {
        unreachable!("list-effect sequence must retain a list tail")
    };
    let right = LazyValue::list_effect_computation_in(
        access.values(),
        "list effect seq",
        ListEffectComputation::Sequence {
            results: tail.clone(),
            continuation: access.values().duplicate_value(continuation),
        },
    );
    Value::List(List::concat(
        List::from_thunk(left.into()),
        List::from_thunk(right.into()),
    ))
}

fn flat_map_result_in(
    access: &EvaluationValueAccess<'_>,
    continuation: &Value,
    head: &Value,
    tail: &Value,
) -> Value {
    let application = LazyValue::from_application_in(
        access.values(),
        access.values().duplicate_value(continuation),
        Arc::from([access.values().duplicate_value(head)]),
    );
    let Value::List(tail) = tail else {
        unreachable!("list-effect flat-map must retain a list tail")
    };
    let right = LazyValue::list_effect_computation_in(
        access.values(),
        "direct list flat-map",
        ListEffectComputation::FlatMapResults {
            results: tail.clone(),
            continuation: access.values().duplicate_value(continuation),
        },
    );
    Value::List(List::concat(
        List::from_thunk(application.into()),
        List::from_thunk(right.into()),
    ))
}

pub(in crate::eval) fn publish_fix_result_in(
    access: &EvaluationValueAccess<'_>,
    handle: &PromisedValue,
    result: Option<(Value, Value)>,
) -> Result<(Value, ManagedPromisePublication), EvaluationHalt> {
    let (assignment, value) = match result {
        None => {
            let empty = Value::List(List::empty());
            (Ok(access.values().duplicate_value(&empty)), empty)
        }
        Some((head, tail)) => {
            let Value::List(tail) = tail else {
                unreachable!("list-effect fix must retain a list tail")
            };
            let assignment = Ok(access.values().duplicate_value(&head));
            let value = Value::List(List::concat(List::from_values(vec![head]), tail));
            (assignment, value)
        }
    };
    let publication = handle
        .publish_in(access.values(), assignment)
        .map_err(|_| EvaluationHalt::new("list effect fix initialized twice"))?;
    Ok((value, publication))
}

fn list_effect_api(_access: &EvaluationValueAccess<'_>) -> Value {
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
