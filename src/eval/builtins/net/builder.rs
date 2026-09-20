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
use crate::number::Number;

use super::super::super::access_machine::{
    AccessMachine, AccessRegionalPoll, RegionalConversionPoll, RegionalKeyConversion,
    RegionalKeyList,
};
use super::super::super::builtin_machine::RegionalBuiltinPoll;
use super::super::super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

static CONTROL_KEY: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["builtin", "interaction_net", "builder", "control"])
});
static SEQUENCE_TAG: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["builtin", "interaction_net", "builder", "sequence"])
});
static CUT_TAG: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "interaction_net", "builder", "cut"]));
static RESET_TAG: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "interaction_net", "builder", "reset"]));
static RESUME_TAG: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["builtin", "interaction_net", "builder", "resume"])
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
            dispatch_return(access, value, state)
        }
        Builtin::InteractionNetBuilderSeq => {
            let [operation, continuation, state] = exact(access, arguments, "builder seq")?;
            let mut state = decode_builder_state(access, &state)?;
            state
                .sequence
                .insert(0, BuilderSequenceFrame::Continue(continuation));
            let state = encode_builder_state(access, state);
            Ok(Value::List(application_list(access, operation, [state])))
        }
        Builtin::InteractionNetBuilderContinue => {
            let [remaining_cuts, outcome] = exact(access, arguments, "builder continuation")?;
            let Value::Number(remaining_cuts) = remaining_cuts else {
                return Err(EvaluationHalt::new(
                    "interaction-net builder cut depth must be a number",
                ));
            };
            let remaining_cuts = remaining_cuts.to_usize_if_integer().ok_or_else(|| {
                EvaluationHalt::new(
                    "interaction-net builder cut depth must be a nonnegative integer",
                )
            })?;
            let [value, state] = decode_outcome(access, &outcome)?;
            Ok(control_stage(access, remaining_cuts, value, state))
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
            let mut state = decode_builder_state(access, &state)?;
            state.sequence.insert(0, BuilderSequenceFrame::Cut);
            let state = encode_builder_state(access, state);
            let results = application_list(access, operation, [state]);
            let selected = deferred_results(
                access,
                "builder cut",
                ListEffectComputation::FirstResult { results },
            );
            let Value::List(selected) = selected else {
                unreachable!("first-result builder recipe must produce a list")
            };
            Ok(deferred_results(
                access,
                "builder cut continuation",
                ListEffectComputation::FlatMapResults {
                    results: selected,
                    continuation: builder_continue(access, 0),
                },
            ))
        }
        Builtin::InteractionNetBuilderResume => {
            let [brand, sequence, resets, value, state] =
                exact(access, arguments, "builder continuation invocation")?;
            resume_continuation(access, brand, sequence, resets, value, state)
        }
        _ => unreachable!("builder composition received another builtin"),
    }
}

pub(super) fn outcome(_access: &RuntimeValueAccess<'_>, value: Value, state: Value) -> Value {
    Value::List(List::from_values(vec![value, state]))
}

fn builder_continue(_access: &RuntimeValueAccess<'_>, remaining_cuts: usize) -> Value {
    Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::InteractionNetBuilderContinue,
        arguments: Arc::from([Value::Number(Number::from_usize(remaining_cuts))]),
    })
}

fn control_stage(
    access: &RuntimeValueAccess<'_>,
    remaining_cuts: usize,
    value: Value,
    state: Value,
) -> Value {
    let results = application_list(
        access,
        Value::Builtin(Builtin::InteractionNetBuilderReturn),
        [value, state],
    );
    if remaining_cuts == 0 {
        return Value::List(results);
    }
    let selected = deferred_results(
        access,
        "builder captured cut",
        ListEffectComputation::FirstResult { results },
    );
    let Value::List(selected) = selected else {
        unreachable!("first-result builder recipe must produce a list")
    };
    deferred_results(
        access,
        "builder captured cut continuation",
        ListEffectComputation::FlatMapResults {
            results: selected,
            continuation: builder_continue(access, remaining_cuts - 1),
        },
    )
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
    key: Option<Box<RegionalKeyConversion>>,
    converted_key: Option<Key>,
    state: Option<Value>,
    state_demand: Option<RegionalWhnfWork>,
    decoded: Option<DecodedBuilderState>,
    access: Option<Box<AccessMachine>>,
    result_demand: Option<RegionalWhnfWork>,
}

enum BuilderStateOperation {
    Get,
    Set { replacement: Value },
    Reset { operation: Value },
    Shift { function: Value },
}

struct DecodedBuilderState {
    brand: Value,
    next_port: Value,
    reverse_operations: Value,
    user_state: Value,
    sequence: Vec<BuilderSequenceFrame>,
}

enum BuilderSequenceFrame {
    Continue(Value),
    Cut,
}

enum BuilderResetFrame {
    Reset { key: Value, sequence: Value },
    Resume { sequence: Value },
}

impl RegionalBuilderBuiltinMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let (path, key, operation, state) = match builtin {
            Builtin::InteractionNetBuilderGet => {
                let [path, state] = arguments else {
                    unreachable!("builder get retains its path and state")
                };
                (
                    Some(access.values().duplicate_value(path)),
                    None,
                    BuilderStateOperation::Get,
                    access.values().duplicate_value(state),
                )
            }
            Builtin::InteractionNetBuilderSet => {
                let [path, replacement, state] = arguments else {
                    unreachable!("builder set retains its path, value, and state")
                };
                (
                    Some(access.values().duplicate_value(path)),
                    None,
                    BuilderStateOperation::Set {
                        replacement: access.values().duplicate_value(replacement),
                    },
                    access.values().duplicate_value(state),
                )
            }
            Builtin::InteractionNetBuilderReset => {
                let [key, operation, state] = arguments else {
                    unreachable!("builder reset retains its key, operation, and state")
                };
                (
                    None,
                    Some(access.values().duplicate_value(key)),
                    BuilderStateOperation::Reset {
                        operation: access.values().duplicate_value(operation),
                    },
                    access.values().duplicate_value(state),
                )
            }
            Builtin::InteractionNetBuilderShift => {
                let [key, function, state] = arguments else {
                    unreachable!("builder shift retains its key, function, and state")
                };
                (
                    None,
                    Some(access.values().duplicate_value(key)),
                    BuilderStateOperation::Shift {
                        function: access.values().duplicate_value(function),
                    },
                    access.values().duplicate_value(state),
                )
            }
            _ => unreachable!("builder state machine received another builtin"),
        };
        Self {
            source_owner,
            operation,
            path: path.map(|path| Box::new(RegionalKeyList::new(access, path, Some(source_owner)))),
            keys: None,
            key: key
                .map(|key| Box::new(RegionalKeyConversion::new(access, key, Some(source_owner)))),
            converted_key: None,
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

        if let Some(key) = &mut self.key {
            return match key.poll_in(access, step_budget) {
                RegionalConversionPoll::Ready(key) => {
                    self.converted_key = Some(key);
                    self.key = None;
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
            BuilderStateOperation::Reset { .. } => self.poll_reset_in(access),
            BuilderStateOperation::Shift { .. } => self.poll_shift_in(access),
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
                BuilderStateOperation::Get
                | BuilderStateOperation::Reset { .. }
                | BuilderStateOperation::Shift { .. } => unreachable!(),
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

    fn poll_reset_in(&mut self, access: &EvaluationValueAccess<'_>) -> RegionalBuiltinPoll {
        let operation = match &self.operation {
            BuilderStateOperation::Reset { operation } => {
                access.values().duplicate_value(operation)
            }
            _ => unreachable!(),
        };
        let key = access.values().values().key_value(
            self.converted_key
                .as_ref()
                .expect("builder reset key conversion must finish"),
        );
        let mut state = self
            .decoded
            .take()
            .expect("builder reset must retain decoded state");
        let outer_sequence =
            encode_sequence_stack(access.values(), std::mem::take(&mut state.sequence));
        let mut resets = match decode_reset_stack(access.values(), &state.user_state) {
            Ok(resets) => resets,
            Err(error) => {
                return RegionalBuiltinPoll::Failed(error.into_permanent_failure());
            }
        };
        resets.push(BuilderResetFrame::Reset {
            key,
            sequence: outer_sequence,
        });
        state.user_state = match replace_reset_stack(access.values(), state.user_state, resets) {
            Ok(state) => state,
            Err(error) => {
                return RegionalBuiltinPoll::Failed(error.into_permanent_failure());
            }
        };
        let state = encode_builder_state(access.values(), state);
        RegionalBuiltinPoll::Ready(Value::List(application_list(
            access.values(),
            operation,
            [state],
        )))
    }

    fn poll_shift_in(&mut self, access: &EvaluationValueAccess<'_>) -> RegionalBuiltinPoll {
        let function = match &self.operation {
            BuilderStateOperation::Shift { function } => access.values().duplicate_value(function),
            _ => unreachable!(),
        };
        let key = access.values().values().key_value(
            self.converted_key
                .as_ref()
                .expect("builder shift key conversion must finish"),
        );
        let mut state = self
            .decoded
            .take()
            .expect("builder shift must retain decoded state");
        let mut resets = match decode_reset_stack(access.values(), &state.user_state) {
            Ok(resets) => resets,
            Err(error) => {
                return RegionalBuiltinPoll::Failed(error.into_permanent_failure());
            }
        };
        let Some(index) = resets.iter().rposition(
            |frame| matches!(frame, BuilderResetFrame::Reset { key: frame_key, .. } if frame_key == &key),
        ) else {
            return RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(
                "`.shift` key is not in reset scope",
            )));
        };
        let inner_resets = resets.split_off(index + 1);
        let target = resets.pop().expect("matching reset frame must exist");
        let BuilderResetFrame::Reset {
            sequence: outer_sequence,
            ..
        } = target
        else {
            unreachable!("shift matching excludes resume frames")
        };
        let captured_sequence =
            encode_sequence_stack(access.values(), std::mem::take(&mut state.sequence));
        let captured_resets = encode_reset_stack(access.values(), inner_resets);
        state.sequence = match decode_sequence_stack(access.values(), &outer_sequence) {
            Ok(sequence) => sequence,
            Err(error) => {
                return RegionalBuiltinPoll::Failed(error.into_permanent_failure());
            }
        };
        state.user_state = match replace_reset_stack(access.values(), state.user_state, resets) {
            Ok(state) => state,
            Err(error) => {
                return RegionalBuiltinPoll::Failed(error.into_permanent_failure());
            }
        };
        let continuation = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderResume,
            arguments: Arc::from([
                access.values().duplicate_value(&state.brand),
                captured_sequence,
                captured_resets,
            ]),
        });
        let operation = Value::Lazy(LazyValue::from_application_in(
            access.values(),
            function,
            Arc::from([continuation]),
        ));
        let state = encode_builder_state(access.values(), state);
        RegionalBuiltinPoll::Ready(Value::List(application_list(
            access.values(),
            operation,
            [state],
        )))
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
        let state = encode_builder_state(
            access.values(),
            DecodedBuilderState {
                user_state: replacement.unwrap_or(state.user_state),
                ..state
            },
        );
        RegionalBuiltinPoll::Ready(Value::List(application_list(
            access.values(),
            Value::Builtin(Builtin::InteractionNetBuilderReturn),
            [value, state],
        )))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(path) = &self.path {
            path.trace_managed_edges(visitor);
        }
        if let Some(key) = &self.key {
            key.trace_managed_edges(visitor);
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
        match &self.operation {
            BuilderStateOperation::Get => {}
            BuilderStateOperation::Set { replacement } => {
                trace_compatibility_value_managed_edges(replacement, visitor);
            }
            BuilderStateOperation::Reset { operation } => {
                trace_compatibility_value_managed_edges(operation, visitor);
            }
            BuilderStateOperation::Shift { function } => {
                trace_compatibility_value_managed_edges(function, visitor);
            }
        }
    }
}

impl DecodedBuilderState {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.brand, visitor);
        trace_compatibility_value_managed_edges(&self.next_port, visitor);
        trace_compatibility_value_managed_edges(&self.reverse_operations, visitor);
        trace_compatibility_value_managed_edges(&self.user_state, visitor);
        for frame in &self.sequence {
            frame.trace_managed_edges(visitor);
        }
    }
}

impl BuilderSequenceFrame {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Self::Continue(continuation) = self {
            trace_compatibility_value_managed_edges(continuation, visitor);
        }
    }
}

fn dispatch_return(
    access: &RuntimeValueAccess<'_>,
    value: Value,
    state: Value,
) -> Result<Value, EvaluationHalt> {
    let mut state = decode_builder_state(access, &state)?;
    if !state.sequence.is_empty() {
        let frame = state.sequence.remove(0);
        let state = encode_builder_state(access, state);
        return match frame {
            BuilderSequenceFrame::Continue(continuation) => {
                let operation = Value::Lazy(LazyValue::from_application_in(
                    access,
                    continuation,
                    Arc::from([value]),
                ));
                Ok(Value::List(application_list(access, operation, [state])))
            }
            BuilderSequenceFrame::Cut => Ok(Value::List(List::from_values(vec![outcome(
                access, value, state,
            )]))),
        };
    }

    let mut resets = decode_reset_stack(access, &state.user_state)?;
    let Some(frame) = resets.pop() else {
        let state = encode_builder_state(access, state);
        return Ok(Value::List(List::from_values(vec![outcome(
            access, value, state,
        )])));
    };
    let sequence = match frame {
        BuilderResetFrame::Reset { sequence, .. } | BuilderResetFrame::Resume { sequence } => {
            decode_sequence_stack(access, &sequence)?
        }
    };
    state.sequence = sequence;
    state.user_state = replace_reset_stack(access, state.user_state, resets)?;
    let state = encode_builder_state(access, state);
    Ok(control_stage(access, 0, value, state))
}

fn resume_continuation(
    access: &RuntimeValueAccess<'_>,
    brand: Value,
    sequence: Value,
    resets: Value,
    value: Value,
    state: Value,
) -> Result<Value, EvaluationHalt> {
    let Value::Opaque(brand) = brand else {
        return Err(EvaluationHalt::new(
            "interaction-net builder continuation brand must be opaque",
        ));
    };
    let captured_brand = super::construction::decode_construction_brand(access.values(), &brand)?;
    let mut state = decode_builder_state(access, &state)?;
    let Value::Opaque(state_brand) = &state.brand else {
        return Err(EvaluationHalt::new(
            "interaction-net builder state brand must be opaque",
        ));
    };
    let state_brand = super::construction::decode_construction_brand(access.values(), state_brand)?;
    if !Arc::ptr_eq(&captured_brand, &state_brand) {
        return Err(EvaluationHalt::new(
            "interaction-net builder continuation belongs to another invocation",
        ));
    }

    let captured_sequence = decode_sequence_stack(access, &sequence)?;
    let captured_resets = decode_reset_stack_value(access, &resets)?;
    let cut_count = sequence_cut_count(&captured_sequence)
        + captured_resets
            .iter()
            .map(|frame| match frame {
                BuilderResetFrame::Reset { sequence, .. }
                | BuilderResetFrame::Resume { sequence } => decode_sequence_stack(access, sequence)
                    .map(|sequence| sequence_cut_count(&sequence)),
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .sum::<usize>();

    let caller_sequence = encode_sequence_stack(access, std::mem::take(&mut state.sequence));
    let mut caller_resets = decode_reset_stack(access, &state.user_state)?;
    caller_resets.push(BuilderResetFrame::Resume {
        sequence: caller_sequence,
    });
    caller_resets.extend(captured_resets);
    state.user_state = replace_reset_stack(access, state.user_state, caller_resets)?;
    state.sequence = captured_sequence;
    let state = encode_builder_state(access, state);
    Ok(control_stage(access, cut_count, value, state))
}

fn sequence_cut_count(sequence: &[BuilderSequenceFrame]) -> usize {
    sequence
        .iter()
        .filter(|frame| matches!(frame, BuilderSequenceFrame::Cut))
        .count()
}

fn encode_builder_state(access: &RuntimeValueAccess<'_>, state: DecodedBuilderState) -> Value {
    let DecodedBuilderState {
        brand,
        next_port,
        reverse_operations,
        user_state,
        sequence,
    } = state;
    let mut fields = vec![brand, next_port, reverse_operations, user_state];
    if !sequence.is_empty() {
        fields.push(encode_sequence_stack(access, sequence));
    }
    Value::List(List::from_values(fields))
}

fn encode_sequence_stack(
    access: &RuntimeValueAccess<'_>,
    sequence: Vec<BuilderSequenceFrame>,
) -> Value {
    Value::List(List::from_values(
        sequence
            .into_iter()
            .map(|frame| match frame {
                BuilderSequenceFrame::Continue(continuation) => {
                    Value::List(List::from_values(vec![
                        access.values().key_value(&SEQUENCE_TAG),
                        continuation,
                    ]))
                }
                BuilderSequenceFrame::Cut => {
                    Value::List(List::from_values(vec![access.values().key_value(&CUT_TAG)]))
                }
            })
            .collect(),
    ))
}

fn decode_sequence_stack(
    access: &RuntimeValueAccess<'_>,
    sequence: &Value,
) -> Result<Vec<BuilderSequenceFrame>, EvaluationHalt> {
    super::netlist::strict_record(access, sequence, "builder sequence stack")?
        .into_iter()
        .map(|frame| {
            let fields = super::netlist::strict_record(access, &frame, "builder sequence frame")?;
            let Some((tag, fields)) = fields.split_first() else {
                return Err(EvaluationHalt::new("builder sequence frame is empty"));
            };
            let Value::Atom(tag) = tag else {
                return Err(EvaluationHalt::new(
                    "builder sequence frame tag must be an atom",
                ));
            };
            if tag.key() == &*SEQUENCE_TAG {
                let [continuation]: [Value; 1] = fields.to_vec().try_into().map_err(|_| {
                    EvaluationHalt::new("builder sequence frame has the wrong number of fields")
                })?;
                Ok(BuilderSequenceFrame::Continue(continuation))
            } else if tag.key() == &*CUT_TAG {
                if !fields.is_empty() {
                    return Err(EvaluationHalt::new(
                        "builder cut frame has the wrong number of fields",
                    ));
                }
                Ok(BuilderSequenceFrame::Cut)
            } else {
                Err(EvaluationHalt::new(
                    "builder sequence frame tag is not recognized",
                ))
            }
        })
        .collect()
}

fn decode_reset_stack(
    access: &RuntimeValueAccess<'_>,
    user_state: &Value,
) -> Result<Vec<BuilderResetFrame>, EvaluationHalt> {
    let Value::Dict(user_state) = user_state else {
        return Err(EvaluationHalt::new(
            "interaction-net builder user state must be a dictionary",
        ));
    };
    let Some(stack) = user_state.get(&*CONTROL_KEY) else {
        return Ok(Vec::new());
    };
    decode_reset_stack_value(access, stack)
}

fn decode_reset_stack_value(
    access: &RuntimeValueAccess<'_>,
    stack: &Value,
) -> Result<Vec<BuilderResetFrame>, EvaluationHalt> {
    super::netlist::strict_record(access, stack, "builder reset stack")?
        .into_iter()
        .map(|frame| {
            let fields = super::netlist::strict_record(access, &frame, "builder reset frame")?;
            let Some((tag, fields)) = fields.split_first() else {
                return Err(EvaluationHalt::new("builder reset frame is empty"));
            };
            let Value::Atom(tag) = tag else {
                return Err(EvaluationHalt::new(
                    "builder reset frame tag must be an atom",
                ));
            };
            if tag.key() == &*RESET_TAG {
                let [key, sequence]: [Value; 2] = fields.to_vec().try_into().map_err(|_| {
                    EvaluationHalt::new("builder reset frame has the wrong number of fields")
                })?;
                decode_sequence_stack(access, &sequence)?;
                Ok(BuilderResetFrame::Reset { key, sequence })
            } else if tag.key() == &*RESUME_TAG {
                let [sequence]: [Value; 1] = fields.to_vec().try_into().map_err(|_| {
                    EvaluationHalt::new("builder resume frame has the wrong number of fields")
                })?;
                decode_sequence_stack(access, &sequence)?;
                Ok(BuilderResetFrame::Resume { sequence })
            } else {
                Err(EvaluationHalt::new(
                    "builder reset frame tag is not recognized",
                ))
            }
        })
        .collect()
}

fn encode_reset_stack(access: &RuntimeValueAccess<'_>, resets: Vec<BuilderResetFrame>) -> Value {
    Value::List(List::from_values(
        resets
            .into_iter()
            .map(|frame| match frame {
                BuilderResetFrame::Reset { key, sequence } => Value::List(List::from_values(vec![
                    access.values().key_value(&RESET_TAG),
                    key,
                    sequence,
                ])),
                BuilderResetFrame::Resume { sequence } => Value::List(List::from_values(vec![
                    access.values().key_value(&RESUME_TAG),
                    sequence,
                ])),
            })
            .collect(),
    ))
}

fn replace_reset_stack(
    access: &RuntimeValueAccess<'_>,
    user_state: Value,
    resets: Vec<BuilderResetFrame>,
) -> Result<Value, EvaluationHalt> {
    let Value::Dict(user_state) = user_state else {
        return Err(EvaluationHalt::new(
            "interaction-net builder user state must be a dictionary",
        ));
    };
    Ok(Value::Dict(user_state.insert(
        CONTROL_KEY.clone(),
        encode_reset_stack(access, resets),
    )))
}

fn decode_builder_state(
    access: &RuntimeValueAccess<'_>,
    state: &Value,
) -> Result<DecodedBuilderState, EvaluationHalt> {
    let mut fields = super::netlist::strict_record(access, state, "builder state")?;
    let sequence = match fields.len() {
        4 => Vec::new(),
        5 => decode_sequence_stack(
            access,
            &fields
                .pop()
                .expect("five-field builder state must retain its sequence"),
        )?,
        _ => {
            return Err(EvaluationHalt::new(
                "builder state has the wrong number of fields",
            ));
        }
    };
    let [brand, next_port, reverse_operations, user_state]: [Value; 4] = fields
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
        sequence,
    })
}
