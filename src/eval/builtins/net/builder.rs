//! Pure state-over-list composition for interaction-net construction.
//!
//! These hidden builtins construct ordinary semantic applications and lazy
//! lists. Ordered search, suspension, and cut remain owned by the canonical
//! list-effect reducer.

use std::sync::{Arc, LazyLock};

use glam_gc::Visitor;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluatedValue, EvaluationFailure, EvaluationHalt, Key, LazyId,
    LazyValue, List, ListEffectComputation, RuntimeValueAccess, Value,
    trace_compatibility_value_managed_edges,
};
use crate::core_net::CoreDataKey;
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};

use super::super::super::access_machine::{
    AccessMachine, AccessRegionalPoll, RegionalConversionPoll, RegionalKeyList,
};
use super::super::super::builtin_machine::RegionalBuiltinPoll;
use super::super::super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

static CONTROL_KEY: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["builtin", "interaction_net", "builder", "control"])
});

#[allow(
    dead_code,
    reason = "PNC2C establishes the initial state before PNC4 constructs it in production"
)]
pub(super) fn initial_user_state(_access: &RuntimeValueAccess<'_>) -> Value {
    Value::Dict(Dict::new_sync().insert(CONTROL_KEY.clone(), Value::List(List::empty())))
}

#[cfg(test)]
pub(super) fn control_key_for_test() -> Key {
    CONTROL_KEY.clone()
}

pub(in crate::eval) fn apply_builder_builtin_in(
    access: &RuntimeValueAccess<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::InteractionNetBuilderReturn => {
            let [value, state] = exact(access, arguments, "builder return")?;
            Ok(Value::List(List::from_values(vec![outcome(
                access, value, state,
            )])))
        }
        Builtin::InteractionNetBuilderSeq => {
            let [operation, continuation, state] = exact(access, arguments, "builder seq")?;
            let results = application_list(access, operation, [state]);
            let continuation = Value::PartialBuiltin(BuiltinCall {
                builtin: Builtin::InteractionNetBuilderContinue,
                arguments: Arc::from([continuation]),
            });
            Ok(deferred_results(
                access,
                "builder seq",
                ListEffectComputation::FlatMapResults {
                    results,
                    continuation,
                },
            ))
        }
        Builtin::InteractionNetBuilderContinue => {
            let [continuation, outcome] = exact(access, arguments, "builder continuation")?;
            let [value, state] = decode_outcome(access, &outcome)?;
            Ok(Value::List(application_list(
                access,
                continuation,
                [value, state],
            )))
        }
        Builtin::InteractionNetBuilderAlt => {
            let [left, right, state] = exact(access, arguments, "builder alt")?;
            let left = application_list(access, left, [access.duplicate_value(&state)]);
            let right = application_list(access, right, [state]);
            Ok(Value::List(List::concat(left, right)))
        }
        Builtin::InteractionNetBuilderFail => {
            let [_state] = exact(access, arguments, "builder fail")?;
            Ok(Value::List(List::empty()))
        }
        Builtin::InteractionNetBuilderCut => {
            let [operation, state] = exact(access, arguments, "builder cut")?;
            let results = application_list(access, operation, [state]);
            Ok(deferred_results(
                access,
                "builder cut",
                ListEffectComputation::FirstResult { results },
            ))
        }
        _ => unreachable!("builder composition received another builtin"),
    }
}

pub(super) fn outcome(_access: &RuntimeValueAccess<'_>, value: Value, state: Value) -> Value {
    Value::List(List::from_values(vec![value, state]))
}

pub(super) fn decode_outcome(
    access: &RuntimeValueAccess<'_>,
    outcome: &Value,
) -> Result<[Value; 2], EvaluationHalt> {
    super::netlist::strict_record(access, outcome, "builder outcome")?
        .try_into()
        .map_err(|_| EvaluationHalt::new("builder outcome has the wrong number of fields"))
}

fn deferred_results(
    access: &RuntimeValueAccess<'_>,
    label: &'static str,
    computation: ListEffectComputation,
) -> Value {
    Value::List(List::from_thunk(
        LazyValue::list_effect_computation_in(access, label, computation).into(),
    ))
}

fn application_list(
    access: &RuntimeValueAccess<'_>,
    function: Value,
    arguments: impl Into<Arc<[Value]>>,
) -> List {
    List::from_thunk(LazyValue::from_application_in(access, function, arguments.into()).into())
}

fn exact<const N: usize>(
    _access: &RuntimeValueAccess<'_>,
    arguments: Vec<Value>,
    operation: &str,
) -> Result<[Value; N], EvaluationHalt> {
    arguments.try_into().map_err(|_| {
        EvaluationHalt::new(format!(
            "interaction-net {operation} received the wrong number of arguments"
        ))
    })
}

pub(in crate::eval) struct RegionalBuilderBuiltinMachine {
    source_owner: LazyId,
    operation: BuilderStateOperation,
    path: Option<Box<RegionalKeyList>>,
    keys: Option<Vec<Key>>,
    state: Option<Value>,
    state_demand: Option<RegionalWhnfWork>,
    decoded: Option<DecodedBuilderState>,
    access: Option<Box<AccessMachine>>,
    result_demand: Option<RegionalWhnfWork>,
}

enum BuilderStateOperation {
    Get,
    Set { replacement: Value },
}

struct DecodedBuilderState {
    brand: Value,
    next_port: Value,
    reverse_operations: Value,
    user_state: Value,
}

impl RegionalBuilderBuiltinMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let (path, operation, state) = match builtin {
            Builtin::InteractionNetBuilderGet => {
                let [path, state] = arguments else {
                    unreachable!("builder get retains its path and state")
                };
                (
                    access.values().duplicate_value(path),
                    BuilderStateOperation::Get,
                    access.values().duplicate_value(state),
                )
            }
            Builtin::InteractionNetBuilderSet => {
                let [path, replacement, state] = arguments else {
                    unreachable!("builder set retains its path, value, and state")
                };
                (
                    access.values().duplicate_value(path),
                    BuilderStateOperation::Set {
                        replacement: access.values().duplicate_value(replacement),
                    },
                    access.values().duplicate_value(state),
                )
            }
            _ => unreachable!("builder state machine received another builtin"),
        };
        Self {
            source_owner,
            operation,
            path: Some(Box::new(RegionalKeyList::new(
                access,
                path,
                Some(source_owner),
            ))),
            keys: None,
            state: Some(state),
            state_demand: None,
            decoded: None,
            access: None,
            result_demand: None,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if let Some(path) = &mut self.path {
            return match path.poll_in(access, step_budget) {
                RegionalConversionPoll::Ready(keys) => {
                    self.keys = Some(keys);
                    self.path = None;
                    RegionalBuiltinPoll::Yielded
                }
                RegionalConversionPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalConversionPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }

        if self.decoded.is_none() {
            let demand = self.state_demand.get_or_insert_with(|| {
                RegionalWhnfWork::from_focus(
                    access,
                    self.state
                        .take()
                        .expect("builder state demand must retain its operand"),
                )
                .with_source_owner(self.source_owner)
            });
            let state =
                match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(state) => state,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
            let state = EvaluatedValue::try_from(state)
                .expect("builder state demand must reach WHNF")
                .into_value();
            self.decoded = match decode_builder_state(access.values(), &state) {
                Ok(state) => Some(state),
                Err(error) => {
                    return RegionalBuiltinPoll::Failed(error.into_permanent_failure());
                }
            };
            self.state_demand = None;
            return RegionalBuiltinPoll::Yielded;
        }

        match &self.operation {
            BuilderStateOperation::Get => self.poll_get_in(access, step_budget),
            BuilderStateOperation::Set { .. } => self.poll_set_in(access, step_budget),
        }
    }

    fn poll_get_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if self
            .keys
            .as_ref()
            .expect("builder path conversion must finish")
            .is_empty()
        {
            let value = access.values().duplicate_value(
                &self
                    .decoded
                    .as_ref()
                    .expect("builder state decoding must finish")
                    .user_state,
            );
            return self.finish_in(access, value, None);
        }

        if let Some(demand) = &mut self.result_demand {
            let value =
                match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
            let value = EvaluatedValue::try_from(value)
                .expect("builder state read must reach WHNF")
                .into_value();
            return self.finish_in(access, value, None);
        }

        if self.access.is_none() {
            let path = self
                .keys
                .as_ref()
                .expect("builder path conversion must finish")
                .iter()
                .cloned()
                .map(CoreDataKey::Key)
                .collect::<Arc<[_]>>();
            let state = access.values().duplicate_value(
                &self
                    .decoded
                    .as_ref()
                    .expect("builder state decoding must finish")
                    .user_state,
            );
            self.access = Some(Box::new(AccessMachine::new_in(
                access,
                self.source_owner,
                path,
                &[state],
            )));
        }
        let machine = self
            .access
            .as_mut()
            .expect("builder state read must install its access machine");
        match machine.poll_in(access, step_budget) {
            AccessRegionalPoll::Ready(value) => self.finish_in(access, value, None),
            AccessRegionalPoll::Whnf(demand) => {
                self.access = None;
                self.result_demand = Some(demand);
                RegionalBuiltinPoll::Yielded
            }
            AccessRegionalPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
            AccessRegionalPoll::Yielded => RegionalBuiltinPoll::Yielded,
            AccessRegionalPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
        }
    }

    fn poll_set_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if self.result_demand.is_none() {
            let replacement = match &self.operation {
                BuilderStateOperation::Set { replacement } => {
                    access.values().duplicate_value(replacement)
                }
                BuilderStateOperation::Get => unreachable!(),
            };
            let keys = self
                .keys
                .as_ref()
                .expect("builder path conversion must finish");
            let update = if keys.is_empty() {
                replacement
            } else {
                let path = Value::List(List::from_values(
                    keys.iter()
                        .map(|key| access.values().values().key_value(key))
                        .collect(),
                ));
                let user_state = access.values().duplicate_value(
                    &self
                        .decoded
                        .as_ref()
                        .expect("builder state decoding must finish")
                        .user_state,
                );
                Value::builtin_call_in(
                    access.values(),
                    Builtin::DictUpdate,
                    vec![path, replacement, user_state],
                )
            };
            self.result_demand = Some(
                RegionalWhnfWork::from_focus(access, update).with_source_owner(self.source_owner),
            );
        }

        let state = match drive_regional_in_place(
            access,
            self.result_demand
                .as_mut()
                .expect("builder state update must install its demand"),
            step_budget,
            reduce_semantic_shell,
        ) {
            RegionalWhnfStatus::Ready(value) => value,
            RegionalWhnfStatus::Boundary(request) => {
                return RegionalBuiltinPoll::Boundary(request);
            }
            RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
            RegionalWhnfStatus::Failed(failure) => {
                return RegionalBuiltinPoll::Failed(failure);
            }
        };
        let state = EvaluatedValue::try_from(state)
            .expect("builder state update must reach WHNF")
            .into_value();
        if !matches!(state, Value::Dict(_)) {
            return RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(
                "interaction-net builder user state must be a dictionary",
            )));
        }
        self.finish_in(access, access.values().values().unit(), Some(state))
    }

    fn finish_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        value: Value,
        replacement: Option<Value>,
    ) -> RegionalBuiltinPoll {
        let state = self
            .decoded
            .take()
            .expect("builder operation must retain decoded state");
        let state = Value::List(List::from_values(vec![
            state.brand,
            state.next_port,
            state.reverse_operations,
            replacement.unwrap_or(state.user_state),
        ]));
        RegionalBuiltinPoll::Ready(Value::List(List::from_values(vec![outcome(
            access.values(),
            value,
            state,
        )])))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(path) = &self.path {
            path.trace_managed_edges(visitor);
        }
        if let Some(state) = &self.state {
            trace_compatibility_value_managed_edges(state, visitor);
        }
        if let Some(demand) = &self.state_demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(state) = &self.decoded {
            state.trace_managed_edges(visitor);
        }
        if let Some(machine) = &self.access {
            machine.trace_managed_edges(visitor);
        }
        if let Some(demand) = &self.result_demand {
            demand.trace_managed_edges(visitor);
        }
        if let BuilderStateOperation::Set { replacement } = &self.operation {
            trace_compatibility_value_managed_edges(replacement, visitor);
        }
    }
}

impl DecodedBuilderState {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.brand, visitor);
        trace_compatibility_value_managed_edges(&self.next_port, visitor);
        trace_compatibility_value_managed_edges(&self.reverse_operations, visitor);
        trace_compatibility_value_managed_edges(&self.user_state, visitor);
    }
}

fn decode_builder_state(
    access: &RuntimeValueAccess<'_>,
    state: &Value,
) -> Result<DecodedBuilderState, EvaluationHalt> {
    let [brand, next_port, reverse_operations, user_state]: [Value; 4] =
        super::netlist::strict_record(access, state, "builder state")?
            .try_into()
            .map_err(|_| EvaluationHalt::new("builder state has the wrong number of fields"))?;
    if !matches!(user_state, Value::Dict(_)) {
        return Err(EvaluationHalt::new(
            "interaction-net builder user state must be a dictionary",
        ));
    }
    Ok(DecodedBuilderState {
        brand,
        next_port,
        reverse_operations,
        user_state,
    })
}
