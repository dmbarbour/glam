//! A pure handler's run (`pure-handler-core`, `pure-handler-control`): a
//! regional reducer in `list.pure`'s family of lazy computations, which
//! threads a state through each branch and keeps every branch, failed ones
//! with their state.
//!
//! The run carries what it returns to, `HandlerControl`: the continuation up
//! to the innermost `.reset`. A `.reset` pushes that continuation, with its
//! key, onto a stack of frames the user's state holds under a key no source
//! can name, as Design.md's reference handler does, so `.get []` captures
//! the stack and `.set []` restores it. A `.shift` takes the frames above its
//! reset as a continuation, and resuming it pushes them back.
//!
//! A cut marks the continuation it runs within. An outcome that returns to
//! the mark stops there, and the cut's walk continues the first to stop. A
//! `.shift` that leaves cuts sends its outcomes past their walks, and
//! resuming a continuation that holds cut marks walks them anew.

use std::sync::{Arc, LazyLock};

use glam_gc::Visitor;

use super::access_machine::{RegionalConversionPoll, RegionalKeyConversion};
use super::effect_machine::effect_function_in;
use super::list_effect_machine::RegionalListEffectPoll;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};
use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluationFailure, EvaluationHalt, HandlerControl, Key, LazyId,
    LazyValue, List, ListEffectComputation, OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue,
    Value, keys, trace_compatibility_value_managed_edges,
};
use crate::core_net::CoreDataKey;
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};
use crate::number::Number;

/// The user's state's reset frames, innermost first.
static RESETS: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "handler", "resets"]));
/// An outcome stopped at a cut's mark: the control after the mark.
static STOP: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "handler", "stop"]));
/// The cut walks an outcome passes before one may take it.
static ESCAPES: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "handler", "escapes"]));

/// The mark of a cut in a continuation.
struct CutMark;

// SAFETY: the mark holds nothing.
unsafe impl OpaquePayloadFamily for CutMark {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("pure handler cut mark", "src/eval/handler_machine.rs");
}

/// A `.reset`'s key, in its frame.
struct ResetKey(Key);

// SAFETY: a key is plain data: no Glam value, runtime root, managed pointer
// or capability.
unsafe impl OpaquePayloadFamily for ResetKey {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("pure handler reset key", "src/eval/handler_machine.rs");
}

/// A frame of the reset stack. Encoded as a list: `[key, sequence, cuts]`
/// for a `.reset`, `[sequence, cuts]` for a resumed continuation's caller,
/// and `[]` for a `.fix`, which hides the frames beneath it.
enum Frame {
    Reset { key: Key, control: HandlerControl },
    Resume(HandlerControl),
    Barrier,
}

impl Frame {
    fn encode(&self, access: &EvaluationValueAccess<'_>) -> Value {
        let control = |control: &HandlerControl| {
            [
                Value::List(control.sequence.clone()),
                Value::Number(Number::from_usize(control.cuts)),
            ]
        };
        Value::List(List::from_values(match self {
            Self::Reset { key, control: rest } => {
                let key =
                    OpaqueValue::new(access.values().values(), Arc::new(ResetKey(key.clone())));
                let mut frame = vec![Value::Opaque(key)];
                frame.extend(control(rest));
                frame
            }
            Self::Resume(rest) => control(rest).into(),
            Self::Barrier => Vec::new(),
        }))
    }

    fn decode(access: &EvaluationValueAccess<'_>, frame: &Value) -> Result<Self, EvaluationHalt> {
        let unknown = || EvaluationHalt::new("a pure handler's reset stack is not its own");
        let Value::List(frame) = frame else {
            return Err(unknown());
        };
        let control = |sequence: &Value, cuts: &Value| match (sequence, cuts) {
            (Value::List(sequence), Value::Number(cuts)) => Ok(HandlerControl {
                sequence: sequence.clone(),
                cuts: cuts.to_usize_if_integer().ok_or_else(unknown)?,
                resets: true,
            }),
            _ => Err(unknown()),
        };
        match frame.value_slice().ok_or_else(unknown)? {
            [] => Ok(Self::Barrier),
            [sequence, cuts] => Ok(Self::Resume(control(sequence, cuts)?)),
            [Value::Opaque(key), sequence, cuts] => Ok(Self::Reset {
                key: key
                    .downcast::<ResetKey>(access.values().values())
                    .ok_or_else(unknown)?
                    .0
                    .clone(),
                control: control(sequence, cuts)?,
            }),
            _ => Err(unknown()),
        }
    }

    fn cuts(&self) -> usize {
        match self {
            Self::Reset { control, .. } | Self::Resume(control) => control.cuts,
            Self::Barrier => 0,
        }
    }
}

/// `control` within a cut: its sequence marked.
fn within_cut(access: &EvaluationValueAccess<'_>, control: &HandlerControl) -> HandlerControl {
    let mark = Value::Opaque(OpaqueValue::new(
        access.values().values(),
        Arc::new(CutMark),
    ));
    HandlerControl {
        sequence: List::concat(List::from_values(vec![mark]), control.sequence.clone()),
        cuts: control.cuts + 1,
        resets: control.resets,
    }
}

fn is_cut_mark(access: &EvaluationValueAccess<'_>, frame: &Value) -> bool {
    matches!(frame, Value::Opaque(mark) if mark.downcast::<CutMark>(access.values().values()).is_some())
}

fn deferred_list_in(
    access: &EvaluationValueAccess<'_>,
    computation: ListEffectComputation,
) -> List {
    List::from_thunk(LazyValue::list_effect_computation_in(access.values(), computation).into())
}

/// The outcomes of `effect` run from `state` with `control` against `api`.
fn run_list_in(
    access: &EvaluationValueAccess<'_>,
    effect: Value,
    state: Value,
    control: HandlerControl,
    api: Value,
) -> List {
    deferred_list_in(
        access,
        ListEffectComputation::HandlerRun {
            effect,
            state,
            control,
            api,
        },
    )
}

/// The outcomes of returning `value` to `control` from `state`.
fn return_list_in(
    access: &EvaluationValueAccess<'_>,
    value: Value,
    state: Value,
    control: HandlerControl,
    api: Value,
) -> List {
    deferred_list_in(
        access,
        ListEffectComputation::HandlerReturn {
            value,
            state,
            control,
            api,
        },
    )
}

/// `outcomes` within `cuts` walks of cuts, innermost first.
fn cut_walks_in(
    access: &EvaluationValueAccess<'_>,
    outcomes: List,
    cuts: usize,
    api: &Value,
) -> List {
    (0..cuts).fold(outcomes, |outcomes, _| {
        deferred_list_in(
            access,
            ListEffectComputation::HandlerCut {
                outcomes,
                api: access.values().duplicate_value(api),
            },
        )
    })
}

fn success(_access: &EvaluationValueAccess<'_>, value: Value, state: Value) -> Value {
    Value::Dict(
        Dict::new_sync()
            .insert(keys::VALUE.clone(), value)
            .insert(keys::STATE.clone(), state),
    )
}

fn one(_access: &EvaluationValueAccess<'_>, outcome: Value) -> List {
    List::from_values(vec![outcome])
}

fn cons_in(_access: &EvaluationValueAccess<'_>, head: Value, rest: List) -> Value {
    Value::List(List::concat(List::from_values(vec![head]), rest))
}

fn failure(message: impl Into<String>) -> RegionalListEffectPoll {
    RegionalListEffectPoll::Failed(Arc::new(EvaluationFailure::message(message.into())))
}

/// What a run waits for next.
enum RunPhase {
    /// The effect, to its function of the API.
    Effect(Value),
    /// The function applied to the API: a request.
    Request(Value),
    /// A request applied to the state: its outcomes.
    Outcomes(Value),
    /// `value` returned to the control's next frame.
    Return {
        value: Value,
        front: RegionalListFront,
    },
    /// A `.reset`'s or `.shift`'s key.
    Key {
        conversion: Box<RegionalKeyConversion>,
        request: Keyed,
    },
    /// The user's state's reset frames, read for `request`.
    Resets { read: Read, request: Framed },
}

/// A request that converts its key first.
enum Keyed {
    Reset { operation: Value },
    Shift { function: Value },
}

/// How far the reset frames are read: the state, then the user's state,
/// then the frames.
enum Read {
    State,
    User(Dict),
    Frames { state: Dict, user: Dict },
}

/// A request that reads the reset frames.
enum Framed {
    /// A return found its sequence empty.
    Pop {
        value: Value,
    },
    Reset {
        key: Key,
        operation: Value,
    },
    Shift {
        key: Key,
        function: Value,
    },
    Resume {
        control: HandlerControl,
        resets: List,
        crossings: usize,
        value: Value,
    },
    Fix {
        function: Value,
    },
}

/// A pure handler's run of an effect, or of a return, from a state.
pub(in crate::eval) struct RegionalHandlerRun {
    source_owner: LazyId,
    phase: RunPhase,
    demand: Option<RegionalWhnfWork>,
    state: Value,
    control: HandlerControl,
    api: Value,
}

impl RegionalHandlerRun {
    pub(in crate::eval) fn run_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        effect: &Value,
        state: &Value,
        control: &HandlerControl,
        api: &Value,
    ) -> Self {
        let values = access.values();
        Self {
            source_owner,
            phase: RunPhase::Effect(values.duplicate_value(effect)),
            demand: None,
            state: values.duplicate_value(state),
            control: control.clone(),
            api: values.duplicate_value(api),
        }
    }

    pub(in crate::eval) fn return_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        value: &Value,
        state: &Value,
        control: &HandlerControl,
        api: &Value,
    ) -> Self {
        let values = access.values();
        let control = control.clone();
        Self {
            source_owner,
            phase: RunPhase::Return {
                value: values.duplicate_value(value),
                front: RegionalListFront::new_in(
                    access,
                    Value::List(control.sequence.clone()),
                    Some(source_owner),
                ),
            },
            demand: None,
            state: values.duplicate_value(state),
            control,
            api: values.duplicate_value(api),
        }
    }

    /// Drives the current demand, made by `make` when there is none.
    fn drive_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
        make: impl FnOnce(&Self) -> RegionalWhnfWork,
    ) -> Result<Value, Box<RegionalListEffectPoll>> {
        if self.demand.is_none() {
            self.demand = Some(make(self));
        }
        let demand = self.demand.as_mut().expect("made above");
        match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
            RegionalWhnfStatus::Ready(value) => {
                self.demand = None;
                Ok(value)
            }
            RegionalWhnfStatus::Boundary(request) => {
                Err(Box::new(RegionalListEffectPoll::Boundary(request)))
            }
            RegionalWhnfStatus::BudgetExhausted => {
                Err(Box::new(RegionalListEffectPoll::BudgetExhausted))
            }
            RegionalWhnfStatus::Failed(failure) => {
                Err(Box::new(RegionalListEffectPoll::Failed(failure)))
            }
        }
    }

    fn focus_in(&self, access: &EvaluationValueAccess<'_>, value: &Value) -> RegionalWhnfWork {
        RegionalWhnfWork::from_focus(access, access.values().duplicate_value(value))
            .with_source_owner(self.source_owner)
    }

    fn apply_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        function: &Value,
        argument: &Value,
    ) -> RegionalWhnfWork {
        RegionalWhnfWork::from_application_checkpoint_in(
            access,
            access.values().duplicate_value(function),
            &[access.values().duplicate_value(argument)],
            Some(self.source_owner),
        )
    }

    fn front_in(&self, access: &EvaluationValueAccess<'_>) -> RegionalListFront {
        RegionalListFront::new_in(
            access,
            Value::List(self.control.sequence.clone()),
            Some(self.source_owner),
        )
    }

    fn run_list_in(&self, access: &EvaluationValueAccess<'_>, effect: &Value) -> List {
        let values = access.values();
        run_list_in(
            access,
            values.duplicate_value(effect),
            values.duplicate_value(&self.state),
            self.control.clone(),
            values.duplicate_value(&self.api),
        )
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalListEffectPoll {
        let values = access.values();
        match &mut self.phase {
            RunPhase::Effect(effect) => {
                let effect = values.duplicate_value(effect);
                let value =
                    match self.drive_in(access, step_budget, |run| run.focus_in(access, &effect)) {
                        Ok(value) => value,
                        Err(poll) => return *poll,
                    };
                match effect_function_in(access, &value, "pure handler") {
                    Ok(function) => {
                        self.phase = RunPhase::Request(function);
                        RegionalListEffectPoll::Continue
                    }
                    Err(error) => RegionalListEffectPoll::Failed(error.into_permanent_failure()),
                }
            }
            RunPhase::Request(function) => {
                let function = values.duplicate_value(function);
                let request = match self.drive_in(access, step_budget, |run| {
                    run.apply_in(access, &function, &run.api)
                }) {
                    Ok(value) => value,
                    Err(poll) => return *poll,
                };
                self.answer_in(access, request)
            }
            RunPhase::Outcomes(request) => {
                let request = values.duplicate_value(request);
                let outcomes = match self.drive_in(access, step_budget, |run| {
                    run.apply_in(access, &request, &run.state)
                }) {
                    Ok(value) => value,
                    Err(poll) => return *poll,
                };
                let Value::List(outcomes) = outcomes else {
                    return failure(format!(
                        "a pure handler's request answers with a list of outcomes, got {:?}",
                        values.diagnostic_debug(&outcomes)
                    ));
                };
                RegionalListEffectPoll::Ready(Value::List(deferred_list_in(
                    access,
                    ListEffectComputation::HandlerContinue {
                        outcomes,
                        control: self.control.clone(),
                        api: values.duplicate_value(&self.api),
                    },
                )))
            }
            RunPhase::Return { value, front } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(None) if !self.control.resets => {
                    RegionalListEffectPoll::Ready(Value::List(one(
                        access,
                        success(
                            access,
                            values.duplicate_value(value),
                            values.duplicate_value(&self.state),
                        ),
                    )))
                }
                RegionalListFrontPoll::Ready(None) => {
                    self.phase = RunPhase::Resets {
                        read: Read::State,
                        request: Framed::Pop {
                            value: values.duplicate_value(value),
                        },
                    };
                    RegionalListEffectPoll::Continue
                }
                RegionalListFrontPoll::Ready(Some((frame, rest))) => {
                    let Value::List(rest) = rest else {
                        unreachable!("a continuation's sequence keeps a list tail")
                    };
                    let value = values.duplicate_value(value);
                    if is_cut_mark(access, &frame) {
                        // The cut's walk continues the first outcome to stop
                        // here.
                        let after = HandlerControl {
                            sequence: rest,
                            cuts: self.control.cuts.saturating_sub(1),
                            resets: self.control.resets,
                        };
                        let stop = success(access, value, values.duplicate_value(&self.state));
                        let Value::Dict(stop) = stop else {
                            unreachable!("an outcome is a dict")
                        };
                        let stop = stop.insert(STOP.clone(), encode_stop(access, &after));
                        return RegionalListEffectPoll::Ready(Value::List(one(
                            access,
                            Value::Dict(stop),
                        )));
                    }
                    self.control.sequence = rest;
                    self.phase = RunPhase::Effect(Value::Lazy(LazyValue::from_application_in(
                        values,
                        frame,
                        Arc::from([value]),
                    )));
                    RegionalListEffectPoll::Continue
                }
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalListEffectPoll::Boundary(request)
                }
                RegionalListFrontPoll::Continue => RegionalListEffectPoll::Continue,
                RegionalListFrontPoll::BudgetExhausted => RegionalListEffectPoll::BudgetExhausted,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            },
            RunPhase::Key {
                conversion,
                request,
            } => {
                let key = match conversion.poll_in(access, step_budget) {
                    RegionalConversionPoll::Ready(key) => key,
                    RegionalConversionPoll::Boundary(request) => {
                        return RegionalListEffectPoll::Boundary(request);
                    }
                    RegionalConversionPoll::Continue => return RegionalListEffectPoll::Continue,
                    RegionalConversionPoll::BudgetExhausted => {
                        return RegionalListEffectPoll::BudgetExhausted;
                    }
                    RegionalConversionPoll::Failed(failure) => {
                        return RegionalListEffectPoll::Failed(failure);
                    }
                };
                let request = match request {
                    Keyed::Reset { operation } => Framed::Reset {
                        key,
                        operation: values.duplicate_value(operation),
                    },
                    Keyed::Shift { function } => Framed::Shift {
                        key,
                        function: values.duplicate_value(function),
                    },
                };
                self.phase = RunPhase::Resets {
                    read: Read::State,
                    request,
                };
                RegionalListEffectPoll::Continue
            }
            RunPhase::Resets { read, .. } => {
                let pending = match read {
                    Read::State => values.duplicate_value(&self.state),
                    Read::User(state) => state
                        .get(&keys::USER)
                        .map(|user| values.duplicate_value(user))
                        .unwrap_or_else(|| Value::Dict(Dict::new_sync())),
                    Read::Frames { user, .. } => match user.get(&RESETS) {
                        Some(frames) => values.duplicate_value(frames),
                        None => Value::List(List::empty()),
                    },
                };
                let value = match self
                    .drive_in(access, step_budget, |run| run.focus_in(access, &pending))
                {
                    Ok(value) => value,
                    Err(poll) => return *poll,
                };
                let RunPhase::Resets { read, request } =
                    std::mem::replace(&mut self.phase, RunPhase::Effect(values.unit()))
                else {
                    unreachable!("matched above")
                };
                let read = match (read, value) {
                    (Read::State, Value::Dict(state)) => Read::User(state),
                    (Read::User(state), Value::Dict(user)) => Read::Frames { state, user },
                    (Read::User(_), _) if matches!(request, Framed::Pop { .. }) => {
                        // A state replaced by one that is no dict holds no
                        // frames.
                        let Framed::Pop { value } = request else {
                            unreachable!("matched above")
                        };
                        return RegionalListEffectPoll::Ready(Value::List(one(
                            access,
                            success(access, value, values.duplicate_value(&self.state)),
                        )));
                    }
                    (Read::State, _) => return failure("a pure handler's state is a dict"),
                    (Read::User(_), _) => {
                        return failure(
                            "`.reset`, `.shift` and `.fix` keep their frames in the user's state, which must be a dict",
                        );
                    }
                    (Read::Frames { state, user }, Value::List(frames)) => {
                        let Some(frames) = frames.value_slice() else {
                            return failure("a pure handler's reset stack is not its own");
                        };
                        let frames = frames
                            .iter()
                            .map(|frame| values.duplicate_value(frame))
                            .collect();
                        return self.framed_in(access, state, user, frames, request);
                    }
                    (Read::Frames { .. }, _) => {
                        return failure("a pure handler's reset stack is not its own");
                    }
                };
                self.phase = RunPhase::Resets { read, request };
                RegionalListEffectPoll::Continue
            }
        }
    }

    /// Answers `request`: the run answers its own, and applies any other to
    /// the state for its outcomes.
    fn answer_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        request: Value,
    ) -> RegionalListEffectPoll {
        let values = access.values();
        let Value::PartialBuiltin(call) = &request else {
            self.phase = RunPhase::Outcomes(request);
            return RegionalListEffectPoll::Continue;
        };
        let duplicate = |value: &Value| values.duplicate_value(value);
        let keyed = |key: &Value, request| RunPhase::Key {
            conversion: Box::new(RegionalKeyConversion::new(
                access,
                duplicate(key),
                Some(self.source_owner),
            )),
            request,
        };
        self.phase = match (call.builtin, &*call.arguments) {
            (Builtin::HandlerReturn, [value]) => RunPhase::Return {
                value: duplicate(value),
                front: self.front_in(access),
            },
            (Builtin::HandlerSeq, [operation, continuation]) => {
                self.control.sequence = List::concat(
                    List::from_values(vec![duplicate(continuation)]),
                    self.control.sequence.clone(),
                );
                RunPhase::Effect(duplicate(operation))
            }
            (Builtin::HandlerAlt, [left, right]) => {
                return RegionalListEffectPoll::Ready(Value::List(List::concat(
                    self.run_list_in(access, left),
                    self.run_list_in(access, right),
                )));
            }
            (Builtin::HandlerCut, [operation]) => {
                let outcomes = run_list_in(
                    access,
                    duplicate(operation),
                    duplicate(&self.state),
                    within_cut(access, &self.control),
                    duplicate(&self.api),
                );
                return RegionalListEffectPoll::Ready(Value::List(cut_walks_in(
                    access, outcomes, 1, &self.api,
                )));
            }
            (Builtin::HandlerFix, [function]) if !self.control.resets => {
                return RegionalListEffectPoll::Ready(Value::List(self.fix_list_in(
                    access,
                    duplicate(function),
                    duplicate(&self.state),
                    false,
                )));
            }
            (Builtin::HandlerFix, [function]) => RunPhase::Resets {
                read: Read::State,
                request: Framed::Fix {
                    function: duplicate(function),
                },
            },
            (Builtin::HandlerReset, [key, operation]) => keyed(
                key,
                Keyed::Reset {
                    operation: duplicate(operation),
                },
            ),
            (Builtin::HandlerShift, [key, function]) => keyed(
                key,
                Keyed::Shift {
                    function: duplicate(function),
                },
            ),
            (Builtin::HandlerResume, [sequence, cuts, resets, crossings, value, _api]) => {
                let (
                    Value::List(sequence),
                    Value::Number(cuts),
                    Value::List(resets),
                    Value::Number(crossings),
                ) = (sequence, cuts, resets, crossings)
                else {
                    return failure("a continuation's parts are not its handler's");
                };
                let (Some(cuts), Some(crossings)) =
                    (cuts.to_usize_if_integer(), crossings.to_usize_if_integer())
                else {
                    return failure("a continuation's parts are not its handler's");
                };
                RunPhase::Resets {
                    read: Read::State,
                    request: Framed::Resume {
                        control: HandlerControl {
                            sequence: sequence.clone(),
                            cuts,
                            resets: true,
                        },
                        resets: resets.clone(),
                        crossings,
                        value: duplicate(value),
                    },
                }
            }
            _ => RunPhase::Outcomes(duplicate(&request)),
        };
        RegionalListEffectPoll::Continue
    }

    /// Answers `request` with the user's state's reset frames, `frames`.
    fn framed_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        state: Dict,
        user: Dict,
        frames: Vec<Value>,
        request: Framed,
    ) -> RegionalListEffectPoll {
        let values = access.values();
        // The state with `frames` in place of the user's.
        let with_frames = |frames: Vec<Value>| {
            let user = if frames.is_empty() {
                user.remove(&RESETS)
            } else {
                user.insert(RESETS.clone(), Value::List(List::from_values(frames)))
            };
            Value::Dict(state.insert(keys::USER.clone(), Value::Dict(user)))
        };
        let decode = |frame: &Value| Frame::decode(access, frame);
        match request {
            Framed::Pop { value } => {
                let Some((top, rest)) = frames.split_first() else {
                    return RegionalListEffectPoll::Ready(Value::List(one(
                        access,
                        success(access, value, values.duplicate_value(&self.state)),
                    )));
                };
                let top = match decode(top) {
                    Ok(top) => top,
                    Err(halt) => {
                        return RegionalListEffectPoll::Failed(halt.into_permanent_failure());
                    }
                };
                self.state = with_frames(
                    rest.iter()
                        .map(|frame| values.duplicate_value(frame))
                        .collect(),
                );
                match top {
                    // A `.fix`'s run ends at its barrier.
                    Frame::Barrier => RegionalListEffectPoll::Ready(Value::List(one(
                        access,
                        success(access, value, values.duplicate_value(&self.state)),
                    ))),
                    Frame::Reset { control, .. } | Frame::Resume(control) => {
                        self.control = control;
                        self.phase = RunPhase::Return {
                            value,
                            front: self.front_in(access),
                        };
                        RegionalListEffectPoll::Continue
                    }
                }
            }
            Framed::Reset { key, operation } => {
                let frame = Frame::Reset {
                    key,
                    control: std::mem::replace(
                        &mut self.control,
                        HandlerControl {
                            sequence: List::empty(),
                            cuts: 0,
                            resets: true,
                        },
                    ),
                };
                let mut pushed = vec![frame.encode(access)];
                pushed.extend(frames);
                self.state = with_frames(pushed);
                self.phase = RunPhase::Effect(operation);
                RegionalListEffectPoll::Continue
            }
            Framed::Shift { key, function } => {
                let mut crossings = self.control.cuts;
                let mut target = None;
                for (index, frame) in frames.iter().enumerate() {
                    match decode(frame) {
                        Ok(Frame::Reset {
                            key: reset,
                            control,
                        }) if reset == key => {
                            target = Some((index, control));
                            break;
                        }
                        Ok(Frame::Barrier) => break,
                        Ok(frame) => crossings += frame.cuts(),
                        Err(halt) => {
                            return RegionalListEffectPoll::Failed(halt.into_permanent_failure());
                        }
                    }
                }
                let Some((index, control)) = target else {
                    return failure("`.shift` key is not in reset scope");
                };
                let continuation = Value::PartialBuiltin(BuiltinCall {
                    builtin: Builtin::HandlerContinuation,
                    arguments: Arc::from([
                        Value::List(self.control.sequence.clone()),
                        Value::Number(Number::from_usize(self.control.cuts)),
                        Value::List(List::from_values(
                            frames[..index]
                                .iter()
                                .map(|frame| values.duplicate_value(frame))
                                .collect(),
                        )),
                        Value::Number(Number::from_usize(crossings)),
                    ]),
                });
                self.state = with_frames(
                    frames[index + 1..]
                        .iter()
                        .map(|frame| values.duplicate_value(frame))
                        .collect(),
                );
                self.control = control;
                let effect = Value::Lazy(LazyValue::from_application_in(
                    values,
                    function,
                    Arc::from([continuation]),
                ));
                if crossings == 0 {
                    self.phase = RunPhase::Effect(effect);
                    return RegionalListEffectPoll::Continue;
                }
                // The outcomes leave the cuts between the shift and its
                // reset.
                RegionalListEffectPoll::Ready(Value::List(deferred_list_in(
                    access,
                    ListEffectComputation::HandlerEscape {
                        outcomes: self.run_list_in(access, &effect),
                        crossings,
                    },
                )))
            }
            Framed::Resume {
                control,
                resets,
                crossings,
                value,
            } => {
                let Some(resets) = resets.value_slice() else {
                    return failure("a continuation's parts are not its handler's");
                };
                let caller = Frame::Resume(std::mem::replace(&mut self.control, control));
                let mut pushed: Vec<Value> = resets
                    .iter()
                    .map(|frame| values.duplicate_value(frame))
                    .collect();
                pushed.push(caller.encode(access));
                pushed.extend(frames);
                self.state = with_frames(pushed);
                if crossings == 0 {
                    self.phase = RunPhase::Return {
                        value,
                        front: self.front_in(access),
                    };
                    return RegionalListEffectPoll::Continue;
                }
                let outcomes = return_list_in(
                    access,
                    value,
                    values.duplicate_value(&self.state),
                    self.control.clone(),
                    values.duplicate_value(&self.api),
                );
                RegionalListEffectPoll::Ready(Value::List(cut_walks_in(
                    access, outcomes, crossings, &self.api,
                )))
            }
            Framed::Fix { function } => {
                let mut pushed = vec![Frame::Barrier.encode(access)];
                pushed.extend(frames);
                let state = with_frames(pushed);
                RegionalListEffectPoll::Ready(Value::List(
                    self.fix_list_in(access, function, state, true),
                ))
            }
        }
    }

    /// `.fix function`'s outcomes, each alternative's run from `state` with
    /// nothing to return to but a barrier, if `resets`, which hides the
    /// frames beneath it; each then returned to the run's control.
    fn fix_list_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        function: Value,
        state: Value,
        resets: bool,
    ) -> List {
        let values = access.values();
        let adapter = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::HandlerFixApply,
            arguments: Arc::from([
                function,
                state,
                values.duplicate_value(&self.api),
                Value::Number(Number::from_usize(usize::from(resets))),
            ]),
        });
        let fixed = deferred_list_in(
            access,
            ListEffectComputation::FixFunction {
                function: adapter,
                alternative: 0,
            },
        );
        deferred_list_in(
            access,
            ListEffectComputation::HandlerContinue {
                outcomes: fixed,
                control: self.control.clone(),
                api: values.duplicate_value(&self.api),
            },
        )
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.phase {
            RunPhase::Effect(value) | RunPhase::Request(value) | RunPhase::Outcomes(value) => {
                trace_compatibility_value_managed_edges(value, visitor);
            }
            RunPhase::Return { value, front } => {
                trace_compatibility_value_managed_edges(value, visitor);
                front.trace_managed_edges(visitor);
            }
            RunPhase::Key {
                conversion,
                request,
            } => {
                conversion.trace_managed_edges(visitor);
                match request {
                    Keyed::Reset { operation: value } | Keyed::Shift { function: value } => {
                        trace_compatibility_value_managed_edges(value, visitor);
                    }
                }
            }
            RunPhase::Resets { read, request } => {
                match read {
                    Read::State => {}
                    Read::User(state) => {
                        trace_compatibility_value_managed_edges(
                            &Value::Dict(state.clone()),
                            visitor,
                        );
                    }
                    Read::Frames { state, user } => {
                        trace_compatibility_value_managed_edges(
                            &Value::Dict(state.clone()),
                            visitor,
                        );
                        trace_compatibility_value_managed_edges(
                            &Value::Dict(user.clone()),
                            visitor,
                        );
                    }
                }
                match request {
                    Framed::Pop { value }
                    | Framed::Reset {
                        operation: value, ..
                    }
                    | Framed::Shift {
                        function: value, ..
                    }
                    | Framed::Fix { function: value } => {
                        trace_compatibility_value_managed_edges(value, visitor);
                    }
                    Framed::Resume {
                        control,
                        resets,
                        value,
                        ..
                    } => {
                        trace_compatibility_value_managed_edges(
                            &Value::List(control.sequence.clone()),
                            visitor,
                        );
                        trace_compatibility_value_managed_edges(
                            &Value::List(resets.clone()),
                            visitor,
                        );
                        trace_compatibility_value_managed_edges(value, visitor);
                    }
                }
            }
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
        trace_compatibility_value_managed_edges(&self.state, visitor);
        trace_compatibility_value_managed_edges(
            &Value::List(self.control.sequence.clone()),
            visitor,
        );
        trace_compatibility_value_managed_edges(&self.api, visitor);
    }
}

fn encode_stop(_access: &EvaluationValueAccess<'_>, control: &HandlerControl) -> Value {
    Value::List(List::from_values(vec![
        Value::List(control.sequence.clone()),
        Value::Number(Number::from_usize(control.cuts)),
        Value::Number(Number::from_usize(usize::from(control.resets))),
    ]))
}

fn decode_stop(_access: &EvaluationValueAccess<'_>, stop: &Value) -> Option<HandlerControl> {
    let Value::List(stop) = stop else {
        return None;
    };
    match stop.value_slice()? {
        [
            Value::List(sequence),
            Value::Number(cuts),
            Value::Number(resets),
        ] => Some(HandlerControl {
            sequence: sequence.clone(),
            cuts: cuts.to_usize_if_integer()?,
            resets: resets.to_usize_if_integer()? != 0,
        }),
        _ => None,
    }
}

/// What a walk of outcomes does with each.
enum Walk {
    /// Returns each success to `control`; passes each failure.
    Continue { control: HandlerControl, api: Value },
    /// Continues the first outcome stopped at this cut, after passing those
    /// before it; ends there.
    Cut { api: Value },
    /// Sends each outcome past `crossings` more cut walks.
    Escape { crossings: usize },
    /// A run's results: its successes, or its failures.
    Outcomes { successes: bool },
}

/// A walk of a pure handler's outcomes, the head's demand with the tail once
/// the front has given them.
pub(in crate::eval) struct RegionalHandlerWalk {
    source_owner: LazyId,
    walk: Walk,
    front: RegionalListFront,
    head: Option<Box<(RegionalWhnfWork, Value)>>,
}

impl RegionalHandlerWalk {
    /// The walk for `recipe`, which is one of the walks.
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        recipe: &ListEffectComputation,
    ) -> Self {
        let values = access.values();
        let (walk, outcomes) = match recipe {
            ListEffectComputation::HandlerContinue {
                outcomes,
                control,
                api,
            } => (
                Walk::Continue {
                    control: control.clone(),
                    api: values.duplicate_value(api),
                },
                outcomes,
            ),
            ListEffectComputation::HandlerCut { outcomes, api } => (
                Walk::Cut {
                    api: values.duplicate_value(api),
                },
                outcomes,
            ),
            ListEffectComputation::HandlerEscape {
                outcomes,
                crossings,
            } => (
                Walk::Escape {
                    crossings: *crossings,
                },
                outcomes,
            ),
            ListEffectComputation::HandlerOutcomes {
                outcomes,
                successes,
            } => (
                Walk::Outcomes {
                    successes: *successes,
                },
                outcomes,
            ),
            _ => unreachable!("only a pure handler's walks are walked here"),
        };
        Self {
            source_owner,
            walk,
            front: RegionalListFront::new_in(
                access,
                Value::List(outcomes.clone()),
                Some(source_owner),
            ),
            head: None,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalListEffectPoll {
        let values = access.values();
        let Some(demand) = self.head.as_deref_mut().map(|(demand, _)| demand) else {
            return match self.front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(None) => {
                    RegionalListEffectPoll::Ready(Value::List(List::empty()))
                }
                RegionalListFrontPoll::Ready(Some((outcome, tail))) => {
                    self.head = Some(Box::new((
                        RegionalWhnfWork::from_focus(access, outcome)
                            .with_source_owner(self.source_owner),
                        tail,
                    )));
                    RegionalListEffectPoll::Continue
                }
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalListEffectPoll::Boundary(request)
                }
                RegionalListFrontPoll::Continue => RegionalListEffectPoll::Continue,
                RegionalListFrontPoll::BudgetExhausted => RegionalListEffectPoll::BudgetExhausted,
                RegionalListFrontPoll::Failed(failure) => RegionalListEffectPoll::Failed(failure),
            };
        };
        let outcome =
            match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
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
        let (_, tail) = *self.head.take().expect("checked above");
        let Value::List(tail) = tail else {
            unreachable!("a pure handler's outcomes keep a list tail")
        };
        let Value::Dict(fields) = &outcome else {
            return failure(format!(
                "a pure handler's outcome is `{{value, state}}` or `{{failed}}`, got {:?}",
                values.diagnostic_debug(&outcome)
            ));
        };
        let failed = fields.get(&keys::FAILED).is_some();
        let escapes = match fields.get(&ESCAPES) {
            Some(Value::Number(escapes)) => escapes.to_usize_if_integer().unwrap_or(0),
            _ => 0,
        };
        let stop = fields.get(&STOP);
        let rest = |walk: ListEffectComputation| deferred_list_in(access, walk);
        match &self.walk {
            Walk::Continue { control, api } => {
                let rest = rest(ListEffectComputation::HandlerContinue {
                    outcomes: tail,
                    control: control.clone(),
                    api: values.duplicate_value(api),
                });
                if failed {
                    return RegionalListEffectPoll::Ready(cons_in(access, outcome, rest));
                }
                let (Some(value), Some(state)) =
                    (fields.get(&keys::VALUE), fields.get(&keys::STATE))
                else {
                    return failure("a pure handler's success has a `value` and a `state`");
                };
                let continued = return_list_in(
                    access,
                    values.duplicate_value(value),
                    values.duplicate_value(state),
                    control.clone(),
                    values.duplicate_value(api),
                );
                RegionalListEffectPoll::Ready(Value::List(List::concat(continued, rest)))
            }
            Walk::Cut { api } => {
                if escapes == 0
                    && let Some(stop) = stop
                {
                    // The first outcome to reach the cut ends it.
                    let (Some(control), Some(value), Some(state)) = (
                        decode_stop(access, stop),
                        fields.get(&keys::VALUE),
                        fields.get(&keys::STATE),
                    ) else {
                        return failure("a pure handler's stopped outcome is not its own");
                    };
                    return RegionalListEffectPoll::Ready(Value::List(return_list_in(
                        access,
                        values.duplicate_value(value),
                        values.duplicate_value(state),
                        control,
                        values.duplicate_value(api),
                    )));
                }
                let passed = if escapes == 0 {
                    outcome
                } else {
                    Value::Dict(with_escapes(access, fields, escapes - 1))
                };
                RegionalListEffectPoll::Ready(cons_in(
                    access,
                    passed,
                    rest(ListEffectComputation::HandlerCut {
                        outcomes: tail,
                        api: values.duplicate_value(api),
                    }),
                ))
            }
            Walk::Escape { crossings } => {
                let crossings = *crossings;
                RegionalListEffectPoll::Ready(cons_in(
                    access,
                    Value::Dict(with_escapes(access, fields, escapes + crossings)),
                    rest(ListEffectComputation::HandlerEscape {
                        outcomes: tail,
                        crossings,
                    }),
                ))
            }
            Walk::Outcomes { .. } if escapes > 0 || stop.is_some() => failure(
                "a continuation returned through a `.cut` that had ended, restored from a saved state",
            ),
            Walk::Outcomes { successes } if failed != *successes => {
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
            // An outcome of the other kind is passed over here, without a
            // list cell of its own.
            Walk::Outcomes { .. } => {
                self.front =
                    RegionalListFront::new_in(access, Value::List(tail), Some(self.source_owner));
                RegionalListEffectPoll::Continue
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.walk {
            Walk::Continue { control, api } => {
                trace_compatibility_value_managed_edges(
                    &Value::List(control.sequence.clone()),
                    visitor,
                );
                trace_compatibility_value_managed_edges(api, visitor);
            }
            Walk::Cut { api } => trace_compatibility_value_managed_edges(api, visitor),
            Walk::Escape { .. } | Walk::Outcomes { .. } => {}
        }
        self.front.trace_managed_edges(visitor);
        if let Some((demand, tail)) = self.head.as_deref() {
            demand.trace_managed_edges(visitor);
            trace_compatibility_value_managed_edges(tail, visitor);
        }
    }
}

fn with_escapes(_access: &EvaluationValueAccess<'_>, fields: &Dict, escapes: usize) -> Dict {
    if escapes == 0 {
        fields.remove(&ESCAPES)
    } else {
        fields.insert(ESCAPES.clone(), Value::Number(Number::from_usize(escapes)))
    }
}

/// `member` of a pure handler's state, lazily.
fn state_member_in(access: &EvaluationValueAccess<'_>, state: &Value, member: &Key) -> Value {
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
/// effect; the rest are the core's requests, each a function of the state,
/// and the adapters of its continuations and fixpoints.
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
    let effect = |builtin, arguments: Vec<Value>| {
        Value::Dict(Dict::new_sync().insert(
            keys::EFF.clone(),
            Value::PartialBuiltin(BuiltinCall {
                builtin,
                arguments: Arc::from(arguments),
            }),
        ))
    };
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
                    .insert(keys::USER.clone(), user)
                    .insert(keys::HANDLER.clone(), handler),
            );
            let outcomes = run_list_in(access, effect, state, HandlerControl::top(), api);
            let view = |successes| {
                Value::List(deferred_list_in(
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
            Ok(Value::List(one(access, success(access, value, state))))
        }
        // A run answers these itself, with its API and its continuation;
        // applied to a state elsewhere, they have neither.
        Builtin::HandlerSeq
        | Builtin::HandlerAlt
        | Builtin::HandlerCut
        | Builtin::HandlerFix
        | Builtin::HandlerReset
        | Builtin::HandlerShift
        | Builtin::HandlerResume => Err(EvaluationHalt::new(
            "`.seq`, `.alt`, `.cut`, `.fix`, `.reset`, `.shift` and a continuation run only within their pure handler",
        )),
        Builtin::HandlerFail => {
            let [state] = exact(arguments, "handler fail")?;
            Ok(Value::List(one(
                access,
                Value::Dict(Dict::new_sync().insert(keys::FAILED.clone(), state)),
            )))
        }
        Builtin::HandlerGet => {
            let [path, state] = exact(arguments, "handler get")?;
            let value = Value::Lazy(LazyValue::from_access_in(
                values,
                Arc::from([CoreDataKey::Key(keys::USER.clone()), CoreDataKey::PathIndex]),
                Arc::from([values.duplicate_value(&state), path]),
            ));
            Ok(Value::List(one(access, success(access, value, state))))
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
            Ok(Value::List(one(
                access,
                success(access, values.unit(), state),
            )))
        }
        // A continuation applied to a value: the effect that resumes it.
        Builtin::HandlerContinuation => {
            let arguments: [Value; 5] = exact(arguments, "handler continuation")?;
            Ok(effect(Builtin::HandlerResume, arguments.into()))
        }
        // One alternative of `.fix`: its effect, then its run.
        Builtin::HandlerFixApply => {
            let arguments: [Value; 5] = exact(arguments, "handler fix")?;
            Ok(effect(Builtin::HandlerFixRun, arguments.into()))
        }
        Builtin::HandlerFixRun => {
            let [function, state, api, resets, future, _list_api] =
                exact(arguments, "handler fix run")?;
            let Value::Number(resets) = resets else {
                return Err(EvaluationHalt::new("a fixpoint's run is not its handler's"));
            };
            let value = state_member_in(access, &future, &keys::VALUE);
            let effect = Value::Lazy(LazyValue::from_application_in(
                values,
                function,
                Arc::from([value]),
            ));
            let control = HandlerControl {
                resets: resets.to_usize_if_integer() != Some(0),
                ..HandlerControl::top()
            };
            Ok(Value::List(run_list_in(
                access, effect, state, control, api,
            )))
        }
        _ => unreachable!("a pure handler's branch received another builtin"),
    }
}

/// The core's requests, which a pure handler's API extends with its own.
/// It holds only builtins, so one dictionary serves every run.
fn handler_api(_access: &EvaluationValueAccess<'_>) -> Value {
    static API: LazyLock<Dict> = LazyLock::new(|| {
        Dict::new_sync()
            .insert((*keys::R).clone(), Value::Builtin(Builtin::HandlerReturn))
            .insert((*keys::SEQ).clone(), Value::Builtin(Builtin::HandlerSeq))
            .insert((*keys::ALT).clone(), Value::Builtin(Builtin::HandlerAlt))
            .insert((*keys::FAIL).clone(), Value::Builtin(Builtin::HandlerFail))
            .insert((*keys::CUT).clone(), Value::Builtin(Builtin::HandlerCut))
            .insert((*keys::GET).clone(), Value::Builtin(Builtin::HandlerGet))
            .insert((*keys::SET).clone(), Value::Builtin(Builtin::HandlerSet))
            .insert((*keys::FIX).clone(), Value::Builtin(Builtin::HandlerFix))
            .insert(
                (*keys::RESET).clone(),
                Value::Builtin(Builtin::HandlerReset),
            )
            .insert(
                (*keys::SHIFT).clone(),
                Value::Builtin(Builtin::HandlerShift),
            )
    });
    Value::Dict(API.clone())
}
