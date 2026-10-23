//! Regional reducer for the closed lazy list-effect recipe family: the list
//! handler, `list.pure`, and pure handlers, which thread a state through
//! each branch and keep every branch, failed ones with their state
//! (`pure-handler-core`).
//!
//! Every poll-spanning semantic edge lives beneath the owning lazy's typed
//! managed checkpoint. Boundary interpretation and terminal publication stay
//! with the outer evaluator route.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluationFailure, EvaluationHalt, LazyId, LazyValue, List,
    ListEffectComputation, ManagedPromisePublication, PromisedValue, Value, keys,
    trace_compatibility_value_managed_edges,
};
use crate::core_net::CoreDataKey;
use crate::evaluation::EvaluationValueAccess;

use super::effect_machine::effect_function_in;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
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
    Continue,
    BudgetExhausted,
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
        alternative: usize,
    },
    Fix {
        function: Value,
        alternative: usize,
        skipped: usize,
        handle: PromisedValue,
        front: RegionalListFront,
    },
    /// A pure handler's run of an effect from `state`.
    HandlerRun {
        phase: HandlerPhase,
        demand: RegionalWhnfWork,
        state: Value,
    },
    /// A walk of a pure handler's outcomes, the head's demand with the tail
    /// once the front has given them.
    HandlerWalk {
        walk: HandlerWalk,
        front: RegionalListFront,
        head: Option<(RegionalWhnfWork, Value)>,
    },
}

enum RunPhase {
    Effect,
    Application,
}

enum HandlerPhase {
    /// The effect, to its function of the API.
    Effect,
    /// The function applied to the API: a request, a function of the
    /// state, or an effect to run in its place.
    Request,
    /// The request applied to the state: its outcomes.
    Outcomes,
}

enum HandlerWalk {
    Sequence { continuation: Value },
    Cut,
    Outcomes { successes: bool },
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
            ListEffectComputation::FixFunction {
                function,
                alternative,
            } => ListEffectState::FixFunction {
                function: RegionalWhnfWork::from_focus(
                    access,
                    access.values().duplicate_value(function),
                )
                .with_source_owner(source_owner),
                alternative: *alternative,
            },
            ListEffectComputation::HandlerRun { effect, state } => ListEffectState::HandlerRun {
                phase: HandlerPhase::Effect,
                demand: RegionalWhnfWork::from_focus(
                    access,
                    access.values().duplicate_value(effect),
                )
                .with_source_owner(source_owner),
                state: access.values().duplicate_value(state),
            },
            ListEffectComputation::HandlerSequence {
                outcomes,
                continuation,
            } => handler_walk_in(
                access,
                source_owner,
                HandlerWalk::Sequence {
                    continuation: access.values().duplicate_value(continuation),
                },
                outcomes,
            ),
            ListEffectComputation::HandlerCut { outcomes } => {
                handler_walk_in(access, source_owner, HandlerWalk::Cut, outcomes)
            }
            ListEffectComputation::HandlerOutcomes {
                outcomes,
                successes,
            } => handler_walk_in(
                access,
                source_owner,
                HandlerWalk::Outcomes {
                    successes: *successes,
                },
                outcomes,
            ),
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
                    RegionalWhnfStatus::BudgetExhausted => {
                        return RegionalListEffectPoll::BudgetExhausted;
                    }
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalListEffectPoll::Failed(failure);
                    }
                };
                match phase {
                    RunPhase::Effect => {
                        let function =
                            match effect_function_in(access, &value, "list effect handler") {
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
                        RegionalListEffectPoll::Continue
                    }
                    RunPhase::Application => {
                        if matches!(value, Value::List(_)) {
                            RegionalListEffectPoll::Ready(value)
                        } else {
                            RegionalListEffectPoll::Failed(Arc::new(EvaluationFailure::message(
                                format!(
                                    "list effect handler expected a standard effect result list, got {:?}",
                                    access.values().diagnostic_debug(&value)
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
                RegionalListFrontPoll::Continue => RegionalListEffectPoll::Continue,
                RegionalListFrontPoll::BudgetExhausted => RegionalListEffectPoll::BudgetExhausted,
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
                RegionalListFrontPoll::Continue => RegionalListEffectPoll::Continue,
                RegionalListFrontPoll::BudgetExhausted => RegionalListEffectPoll::BudgetExhausted,
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
                RegionalListFrontPoll::Continue => RegionalListEffectPoll::Continue,
                RegionalListFrontPoll::BudgetExhausted => RegionalListEffectPoll::BudgetExhausted,
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
                RegionalListFrontPoll::Continue => RegionalListEffectPoll::Continue,
                RegionalListFrontPoll::BudgetExhausted => RegionalListEffectPoll::BudgetExhausted,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            },
            ListEffectState::FixFunction {
                function,
                alternative,
            } => {
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
                    RegionalWhnfStatus::BudgetExhausted => {
                        return RegionalListEffectPoll::BudgetExhausted;
                    }
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalListEffectPoll::Failed(failure);
                    }
                };
                let handle = access
                    .values()
                    .construct_managed_promise()
                    .expect("managed promise representation must fit one collector run");
                let operation = LazyValue::from_application_in(
                    access.values(),
                    access.values().duplicate_value(&function),
                    Arc::from([Value::Promised(handle.duplicate_in(access.values()))]),
                );
                let results = deferred_run_list_in(access, &Value::Lazy(operation));
                self.state = ListEffectState::Fix {
                    function,
                    alternative: *alternative,
                    skipped: 0,
                    handle,
                    front: RegionalListFront::new_in(access, results, Some(self.source_owner)),
                };
                RegionalListEffectPoll::Continue
            }
            ListEffectState::Fix {
                function,
                alternative,
                skipped,
                handle,
                front,
            } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(Some((_head, tail))) if *skipped < *alternative => {
                    *skipped += 1;
                    *front = RegionalListFront::new_in(access, tail, Some(self.source_owner));
                    RegionalListEffectPoll::Continue
                }
                RegionalListFrontPoll::Ready(Some((head, _tail))) => {
                    let next_alternative = match alternative.checked_add(1) {
                        Some(next) => next,
                        None => {
                            return RegionalListEffectPoll::Failed(Arc::new(
                                EvaluationFailure::message(
                                    "list effect fix alternative count exhausted",
                                ),
                            ));
                        }
                    };
                    let tail = List::from_thunk(
                        LazyValue::list_effect_computation_in(
                            access.values(),
                            ListEffectComputation::FixFunction {
                                function: access.values().duplicate_value(function),
                                alternative: next_alternative,
                            },
                        )
                        .into(),
                    );
                    RegionalListEffectPoll::FixReady {
                        handle: handle.duplicate_in(access.values()),
                        result: Some((head, Value::List(tail))),
                    }
                }
                RegionalListFrontPoll::Ready(None) => RegionalListEffectPoll::FixReady {
                    handle: handle.duplicate_in(access.values()),
                    result: None,
                },
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalListEffectPoll::Boundary(request)
                }
                RegionalListFrontPoll::Continue => RegionalListEffectPoll::Continue,
                RegionalListFrontPoll::BudgetExhausted => RegionalListEffectPoll::BudgetExhausted,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            },
            ListEffectState::HandlerRun {
                phase,
                demand,
                state,
            } => {
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
                    RegionalWhnfStatus::BudgetExhausted => {
                        return RegionalListEffectPoll::BudgetExhausted;
                    }
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalListEffectPoll::Failed(failure);
                    }
                };
                // An effect that a request answers with runs in its place.
                let effect = match phase {
                    HandlerPhase::Effect => true,
                    HandlerPhase::Request => {
                        matches!(&value, Value::Dict(dict) if dict.get(&keys::EFF).is_some())
                    }
                    HandlerPhase::Outcomes => false,
                };
                if effect {
                    let function = match effect_function_in(access, &value, "pure handler") {
                        Ok(function) => function,
                        Err(error) => {
                            return RegionalListEffectPoll::Failed(error.into_permanent_failure());
                        }
                    };
                    let api = state_member_in(access, state, &keys::API);
                    *demand = RegionalWhnfWork::from_application_checkpoint_in(
                        access,
                        function,
                        &[api],
                        Some(self.source_owner),
                    );
                    *phase = HandlerPhase::Request;
                    return RegionalListEffectPoll::Continue;
                }
                match phase {
                    HandlerPhase::Request => {
                        *demand = RegionalWhnfWork::from_application_checkpoint_in(
                            access,
                            value,
                            &[access.values().duplicate_value(state)],
                            Some(self.source_owner),
                        );
                        *phase = HandlerPhase::Outcomes;
                        RegionalListEffectPoll::Continue
                    }
                    _ if matches!(value, Value::List(_)) => RegionalListEffectPoll::Ready(value),
                    _ => RegionalListEffectPoll::Failed(Arc::new(EvaluationFailure::message(
                        format!(
                            "a pure handler's request answers with a list of outcomes, got {:?}",
                            access.values().diagnostic_debug(&value)
                        ),
                    ))),
                }
            }
            ListEffectState::HandlerWalk { walk, front, head } => {
                let Some((demand, _)) = head else {
                    return match front.poll_in(access, step_budget) {
                        RegionalListFrontPoll::Ready(None) => {
                            RegionalListEffectPoll::Ready(Value::List(List::empty()))
                        }
                        RegionalListFrontPoll::Ready(Some((outcome, tail))) => {
                            *head = Some((
                                RegionalWhnfWork::from_focus(access, outcome)
                                    .with_source_owner(self.source_owner),
                                tail,
                            ));
                            RegionalListEffectPoll::Continue
                        }
                        RegionalListFrontPoll::Boundary(request) => {
                            RegionalListEffectPoll::Boundary(request)
                        }
                        RegionalListFrontPoll::Continue => RegionalListEffectPoll::Continue,
                        RegionalListFrontPoll::BudgetExhausted => {
                            RegionalListEffectPoll::BudgetExhausted
                        }
                        RegionalListFrontPoll::Failed(failure) => {
                            RegionalListEffectPoll::Failed(failure)
                        }
                    };
                };
                let outcome = match drive_regional_in_place(
                    access,
                    demand,
                    step_budget,
                    reduce_semantic_shell,
                ) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalListEffectPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::BudgetExhausted => {
                        return RegionalListEffectPoll::BudgetExhausted;
                    }
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalListEffectPoll::Failed(failure);
                    }
                };
                let (_, tail) = head.take().expect("checked above");
                let Value::List(tail) = tail else {
                    unreachable!("a pure handler's outcomes keep a list tail")
                };
                let Value::Dict(fields) = &outcome else {
                    return RegionalListEffectPoll::Failed(Arc::new(EvaluationFailure::message(
                        format!(
                            "a pure handler's outcome is `{{value, state}}` or `{{failed}}`, got {:?}",
                            access.values().diagnostic_debug(&outcome)
                        ),
                    )));
                };
                let failed = fields.get(&keys::FAILED).is_some();
                let rest = |walk: ListEffectComputation| deferred_handler_list_in(access, walk);
                match walk {
                    HandlerWalk::Sequence { continuation } => {
                        let rest = rest(ListEffectComputation::HandlerSequence {
                            outcomes: tail,
                            continuation: access.values().duplicate_value(continuation),
                        });
                        if failed {
                            return RegionalListEffectPoll::Ready(cons_in(access, outcome, rest));
                        }
                        let (Some(value), Some(state)) =
                            (fields.get(&keys::VALUE), fields.get(&keys::STATE))
                        else {
                            return RegionalListEffectPoll::Failed(Arc::new(
                                EvaluationFailure::message(
                                    "a pure handler's success has a `value` and a `state`",
                                ),
                            ));
                        };
                        let next = LazyValue::from_application_in(
                            access.values(),
                            access.values().duplicate_value(continuation),
                            Arc::from([access.values().duplicate_value(value)]),
                        );
                        let continued = handler_run_in(
                            access,
                            Value::Lazy(next),
                            access.values().duplicate_value(state),
                        );
                        RegionalListEffectPoll::Ready(Value::List(List::concat(continued, rest)))
                    }
                    HandlerWalk::Cut if failed => RegionalListEffectPoll::Ready(cons_in(
                        access,
                        outcome,
                        rest(ListEffectComputation::HandlerCut { outcomes: tail }),
                    )),
                    // The first success ends the cut.
                    HandlerWalk::Cut => {
                        RegionalListEffectPoll::Ready(Value::List(List::from_values(vec![outcome])))
                    }
                    HandlerWalk::Outcomes { successes } if failed != *successes => {
                        let successes = *successes;
                        let result = handler_result_in(access, fields, successes);
                        RegionalListEffectPoll::Ready(cons_in(
                            access,
                            result,
                            rest(ListEffectComputation::HandlerOutcomes {
                                outcomes: tail,
                                successes,
                            }),
                        ))
                    }
                    // An outcome of the other kind is passed over here,
                    // without a list cell of its own.
                    HandlerWalk::Outcomes { .. } => {
                        *front = RegionalListFront::new_in(
                            access,
                            Value::List(tail),
                            Some(self.source_owner),
                        );
                        RegionalListEffectPoll::Continue
                    }
                }
            }
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
            Self::FixFunction { function, .. } => function.trace_managed_edges(visitor),
            Self::Fix {
                function,
                handle,
                front,
                ..
            } => {
                trace_compatibility_value_managed_edges(function, visitor);
                handle.trace_managed_edge(visitor);
                front.trace_managed_edges(visitor);
            }
            Self::HandlerRun { demand, state, .. } => {
                demand.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(state, visitor);
            }
            Self::HandlerWalk { walk, front, head } => {
                if let HandlerWalk::Sequence { continuation } = walk {
                    trace_compatibility_value_managed_edges(continuation, visitor);
                }
                front.trace_managed_edges(visitor);
                if let Some((demand, tail)) = head {
                    demand.trace_managed_edges(visitor);
                    trace_compatibility_value_managed_edges(tail, visitor);
                }
            }
        }
    }
}

fn handler_walk_in(
    access: &EvaluationValueAccess<'_>,
    source_owner: LazyId,
    walk: HandlerWalk,
    outcomes: &List,
) -> ListEffectState {
    ListEffectState::HandlerWalk {
        walk,
        front: RegionalListFront::new_in(access, Value::List(outcomes.clone()), Some(source_owner)),
        head: None,
    }
}

fn deferred_handler_list_in(
    access: &EvaluationValueAccess<'_>,
    computation: ListEffectComputation,
) -> List {
    List::from_thunk(LazyValue::list_effect_computation_in(access.values(), computation).into())
}

/// The outcomes of `effect` run from `state`.
fn handler_run_in(access: &EvaluationValueAccess<'_>, effect: Value, state: Value) -> List {
    deferred_handler_list_in(access, ListEffectComputation::HandlerRun { effect, state })
}

fn cons_in(access: &EvaluationValueAccess<'_>, head: Value, rest: List) -> Value {
    let _ = access;
    Value::List(List::concat(List::from_values(vec![head]), rest))
}

/// `member` of a pure handler's state, lazily.
fn state_member_in(
    access: &EvaluationValueAccess<'_>,
    state: &Value,
    member: &crate::core::Key,
) -> Value {
    Value::Lazy(LazyValue::from_access_in(
        access.values(),
        Arc::from([CoreDataKey::Key(member.clone())]),
        Arc::from([access.values().duplicate_value(state)]),
    ))
}

/// A run's result for one of its outcomes: a success's value with the
/// user's state and the handler's, or a failure's states.
fn handler_result_in(access: &EvaluationValueAccess<'_>, outcome: &Dict, success: bool) -> Value {
    let undefined = || Value::Dict(Dict::new_sync());
    let state = outcome
        .get(if success { &keys::STATE } else { &keys::FAILED })
        .map(|state| access.values().duplicate_value(state))
        .unwrap_or_else(undefined);
    let mut result = Dict::new_sync()
        .insert(
            keys::USER.clone(),
            state_member_in(access, &state, &keys::USER),
        )
        .insert(
            keys::HANDLER.clone(),
            state_member_in(access, &state, &keys::HANDLER),
        );
    if success {
        let value = outcome
            .get(&keys::VALUE)
            .map(|value| access.values().duplicate_value(value))
            .unwrap_or_else(undefined);
        result = result.insert(keys::VALUE.clone(), value);
    }
    Value::Dict(result)
}

/// Applies a pure handler's builtin (`pure-handler-core`): `Handler` runs an
/// effect; the rest are the core's requests, each a function of the state.
pub(in crate::eval) fn apply_handler_builtin_in(
    access: &EvaluationValueAccess<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    fn exact<const N: usize>(
        arguments: Vec<Value>,
        name: &str,
    ) -> Result<[Value; N], EvaluationHalt> {
        arguments.try_into().map_err(|_| {
            EvaluationHalt::new(format!(
                "{name} builtin received the wrong number of arguments"
            ))
        })
    }
    let values = access.values();
    let success = |value: Value, state: Value| {
        Value::Dict(
            Dict::new_sync()
                .insert(keys::VALUE.clone(), value)
                .insert(keys::STATE.clone(), state),
        )
    };
    let one = |outcome| Ok(Value::List(List::from_values(vec![outcome])));
    match builtin {
        Builtin::Handler => {
            let [ops, user, handler, effect] = exact(arguments, "handler")?;
            let api = Value::Lazy(LazyValue::from_builtin_in(
                values,
                BuiltinCall {
                    builtin: Builtin::DictUnion,
                    arguments: Arc::from([handler_api(access), ops]),
                },
            ));
            let state = Value::Dict(
                Dict::new_sync()
                    .insert(keys::API.clone(), api)
                    .insert(keys::USER.clone(), user)
                    .insert(keys::HANDLER.clone(), handler),
            );
            let outcomes = handler_run_in(access, effect, state);
            let view = |successes| {
                Value::List(deferred_handler_list_in(
                    access,
                    ListEffectComputation::HandlerOutcomes {
                        outcomes: outcomes.clone(),
                        successes,
                    },
                ))
            };
            Ok(Value::Dict(
                Dict::new_sync()
                    .insert(keys::OK.clone(), view(true))
                    .insert(keys::FAILED.clone(), view(false)),
            ))
        }
        Builtin::HandlerReturn => {
            let [value, state] = exact(arguments, "handler return")?;
            one(success(value, state))
        }
        Builtin::HandlerSeq => {
            let [operation, continuation, state] = exact(arguments, "handler seq")?;
            Ok(Value::List(deferred_handler_list_in(
                access,
                ListEffectComputation::HandlerSequence {
                    outcomes: handler_run_in(access, operation, state),
                    continuation,
                },
            )))
        }
        Builtin::HandlerAlt => {
            let [left, right, state] = exact(arguments, "handler alt")?;
            let left = handler_run_in(access, left, values.duplicate_value(&state));
            Ok(Value::List(List::concat(
                left,
                handler_run_in(access, right, state),
            )))
        }
        Builtin::HandlerFail => {
            let [state] = exact(arguments, "handler fail")?;
            one(Value::Dict(
                Dict::new_sync().insert(keys::FAILED.clone(), state),
            ))
        }
        Builtin::HandlerCut => {
            let [operation, state] = exact(arguments, "handler cut")?;
            Ok(Value::List(deferred_handler_list_in(
                access,
                ListEffectComputation::HandlerCut {
                    outcomes: handler_run_in(access, operation, state),
                },
            )))
        }
        Builtin::HandlerGet => {
            let [path, state] = exact(arguments, "handler get")?;
            let value = Value::Lazy(LazyValue::from_access_in(
                values,
                Arc::from([CoreDataKey::Key(keys::USER.clone()), CoreDataKey::PathIndex]),
                Arc::from([values.duplicate_value(&state), path]),
            ));
            one(success(value, state))
        }
        Builtin::HandlerSet => {
            let [path, item, state] = exact(arguments, "handler set")?;
            // The user's state at `path` is the state at `[user] ++ path`.
            let path = super::sequence::append_values(
                values,
                Value::List(List::from_values(vec![keys::USER.to_value_in(values)])),
                path,
            )?;
            let state = Value::Lazy(LazyValue::from_builtin_in(
                values,
                BuiltinCall {
                    builtin: Builtin::DictUpdate,
                    arguments: Arc::from([path, item, state]),
                },
            ));
            one(success(values.unit(), state))
        }
        _ => unreachable!("a pure handler's branch received another builtin"),
    }
}

/// The core's requests, which a pure handler's API extends with its own.
/// It holds only builtins, so one dictionary serves every run.
fn handler_api(_access: &EvaluationValueAccess<'_>) -> Value {
    static API: std::sync::LazyLock<Dict> = std::sync::LazyLock::new(|| {
        Dict::new_sync()
            .insert((*keys::R).clone(), Value::Builtin(Builtin::HandlerReturn))
            .insert((*keys::SEQ).clone(), Value::Builtin(Builtin::HandlerSeq))
            .insert((*keys::ALT).clone(), Value::Builtin(Builtin::HandlerAlt))
            .insert((*keys::FAIL).clone(), Value::Builtin(Builtin::HandlerFail))
            .insert((*keys::CUT).clone(), Value::Builtin(Builtin::HandlerCut))
            .insert((*keys::GET).clone(), Value::Builtin(Builtin::HandlerGet))
            .insert((*keys::SET).clone(), Value::Builtin(Builtin::HandlerSet))
    });
    Value::Dict(API.clone())
}

fn deferred_run_list_in(access: &EvaluationValueAccess<'_>, operation: &Value) -> Value {
    let lazy = LazyValue::list_effect_computation_in(
        access.values(),
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
        ListEffectComputation::Run {
            effect: Value::Lazy(application),
        },
    );
    let Value::List(tail) = tail else {
        unreachable!("list-effect sequence must retain a list tail")
    };
    let right = LazyValue::list_effect_computation_in(
        access.values(),
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

/// The list handler's API. It holds only builtins and the empty list, no
/// managed value, so one dictionary serves every run.
fn list_effect_api(_access: &EvaluationValueAccess<'_>) -> Value {
    static API: std::sync::LazyLock<Dict> = std::sync::LazyLock::new(|| {
        Dict::new_sync()
            .insert(
                (*keys::R).clone(),
                Value::Builtin(Builtin::ListEffectReturn),
            )
            .insert((*keys::SEQ).clone(), Value::Builtin(Builtin::ListEffectSeq))
            .insert((*keys::ALT).clone(), Value::Builtin(Builtin::ListEffectAlt))
            .insert((*keys::FAIL).clone(), Value::List(List::empty()))
            .insert((*keys::CUT).clone(), Value::Builtin(Builtin::ListEffectCut))
            .insert((*keys::FIX).clone(), Value::Builtin(Builtin::ListEffectFix))
    });
    Value::Dict(API.clone())
}
