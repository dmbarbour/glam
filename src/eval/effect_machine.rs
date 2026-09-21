//! Regional effect dispatch and fixpoint construction.
//!
//! Effect builtins construct ordinary semantic effect recipes. They do not
//! interpret host callbacks here, so every poll-spanning edge can remain raw
//! beneath the owning lazy's managed builtin checkpoint.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Builtin, BuiltinCall, EvaluationFailure, EvaluationHalt, FixpointComputation, Key, LazyId,
    LazyValue, List, Value, keys, trace_compatibility_value_managed_edges,
};
use crate::core_net::CoreDataKey;
use crate::evaluation::EvaluationValueAccess;

use super::access_machine::{RegionalConversionPoll, RegionalKeyConversion};
use super::builtin_machine::RegionalBuiltinPoll;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::value::is_undefined_dict_value;
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalEffectMachine {
    phase: EffectPhase,
    source_owner: LazyId,
}

pub(in crate::eval) fn effect_function_in(
    access: &EvaluationValueAccess<'_>,
    effect: &Value,
    purpose: &str,
) -> Result<Value, EvaluationHalt> {
    let Value::Dict(dict) = effect else {
        return Err(EvaluationHalt::new(format!(
            "{purpose} requires an effect dictionary, got {effect:?}"
        )));
    };
    let Some(function) = dict.get(&*keys::EFF) else {
        return Err(EvaluationHalt::new(format!(
            "{purpose} requires an `eff` member"
        )));
    };
    if is_undefined_dict_value(access.values(), function) {
        return Err(EvaluationHalt::new(format!(
            "{purpose} requires an `eff` member"
        )));
    }
    Ok(access.values().duplicate_value(function))
}

enum EffectPhase {
    Apply {
        function: RegionalWhnfWork,
        argument: Value,
        api: Value,
    },
    CallName {
        name: RegionalKeyConversion,
        arguments: Value,
        api: Value,
    },
    CallArguments {
        name: Key,
        arguments: RegionalWhnfWork,
        api: Value,
    },
    CallItems {
        name: Key,
        items: RegionalListFront,
        arguments: Vec<Value>,
        api: Value,
    },
    Fixpoint {
        function: RegionalWhnfWork,
    },
    Map {
        function: Value,
        items: Value,
    },
    MapItems {
        function: Value,
        items: RegionalWhnfWork,
        results: Value,
        api: Value,
    },
    MapResults {
        function: Value,
        items: Value,
        results: RegionalWhnfWork,
        api: Value,
    },
    MapFront {
        function: Value,
        items: RegionalListFront,
        results: Value,
        api: Value,
    },
    MapContinue {
        function: Value,
        items: Value,
        results: Value,
        result: Value,
    },
}

impl RegionalEffectMachine {
    pub(in crate::eval) fn supports(builtin: Builtin) -> bool {
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

    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let duplicate = |value: &Value| access.values().duplicate_value(value);
        let demand = |value: &Value| {
            RegionalWhnfWork::from_focus(access, duplicate(value)).with_source_owner(source_owner)
        };
        let phase = match builtin {
            Builtin::EffectApply => {
                let [function, argument, api] = arguments else {
                    unreachable!("effect apply retains three operands")
                };
                EffectPhase::Apply {
                    function: demand(function),
                    argument: duplicate(argument),
                    api: duplicate(api),
                }
            }
            Builtin::EffectCall => {
                let [name, arguments, api] = arguments else {
                    unreachable!("effect call retains three operands")
                };
                EffectPhase::CallName {
                    name: RegionalKeyConversion::new(access, duplicate(name), Some(source_owner)),
                    arguments: duplicate(arguments),
                    api: duplicate(api),
                }
            }
            Builtin::Fixpoint => {
                let [function] = arguments else {
                    unreachable!("fixpoint retains one operand")
                };
                EffectPhase::Fixpoint {
                    function: demand(function),
                }
            }
            Builtin::EffectMap => {
                let [function, items] = arguments else {
                    unreachable!("effect map retains two operands")
                };
                EffectPhase::Map {
                    function: duplicate(function),
                    items: duplicate(items),
                }
            }
            Builtin::EffectMapRun => {
                let [function, items, results, api] = arguments else {
                    unreachable!("effect map run retains four operands")
                };
                EffectPhase::MapItems {
                    function: duplicate(function),
                    items: demand(items),
                    results: duplicate(results),
                    api: duplicate(api),
                }
            }
            Builtin::EffectMapContinue => {
                let [function, items, results, result] = arguments else {
                    unreachable!("effect map continuation retains four operands")
                };
                EffectPhase::MapContinue {
                    function: duplicate(function),
                    items: duplicate(items),
                    results: duplicate(results),
                    result: duplicate(result),
                }
            }
            _ => unreachable!("effect machine received another builtin"),
        };
        Self {
            phase,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match &mut self.phase {
            EffectPhase::Apply {
                function,
                argument,
                api,
            } => {
                let function = match poll_whnf_in(access, function, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                RegionalBuiltinPoll::Ready(application_in(access, function, &[api, argument]))
            }
            EffectPhase::CallName {
                name,
                arguments,
                api,
            } => {
                let name = match name.poll_in(access, step_budget) {
                    RegionalConversionPoll::Ready(name) => name,
                    RegionalConversionPoll::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalConversionPoll::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalConversionPoll::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
                self.phase = EffectPhase::CallArguments {
                    name,
                    arguments: RegionalWhnfWork::from_focus(
                        access,
                        access.values().duplicate_value(arguments),
                    )
                    .with_source_owner(self.source_owner),
                    api: access.values().duplicate_value(api),
                };
                RegionalBuiltinPoll::Yielded
            }
            EffectPhase::CallArguments {
                name,
                arguments,
                api,
            } => {
                let arguments = match poll_whnf_in(access, arguments, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                if !matches!(arguments, Value::List(_)) {
                    return failure("effect call builtin requires a list of arguments");
                }
                self.phase = EffectPhase::CallItems {
                    name: name.clone(),
                    items: RegionalListFront::new_in(access, arguments, Some(self.source_owner)),
                    arguments: Vec::new(),
                    api: access.values().duplicate_value(api),
                };
                RegionalBuiltinPoll::Yielded
            }
            EffectPhase::CallItems {
                name,
                items,
                arguments,
                api,
            } => match items.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(Some((argument, tail))) => {
                    arguments.push(argument);
                    *items = RegionalListFront::new_in(access, tail, Some(self.source_owner));
                    RegionalBuiltinPoll::Yielded
                }
                RegionalListFrontPoll::Ready(None) => {
                    RegionalBuiltinPoll::Ready(effect_call_in(access, api, name, arguments))
                }
                RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalListFrontPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            },
            EffectPhase::Fixpoint { function } => {
                let function = match poll_whnf_in(access, function, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                if !matches!(function, Value::Function(_) | Value::Net(_)) {
                    return failure("fixpoint builtin requires a function value");
                }
                let lazy = LazyValue::computed_fixpoint_in(
                    access.values(),
                    "fixpoint",
                    FixpointComputation::Function(function),
                );
                RegionalBuiltinPoll::Ready(Value::Lazy(lazy))
            }
            EffectPhase::Map { function, items } => {
                RegionalBuiltinPoll::Ready(effect_map_in(access, function, items))
            }
            EffectPhase::MapItems {
                function,
                items,
                results,
                api,
            } => {
                let items = match poll_whnf_in(access, items, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                if !matches!(items, Value::List(_)) {
                    return failure("effect map requires a list");
                }
                self.phase = EffectPhase::MapResults {
                    function: access.values().duplicate_value(function),
                    items,
                    results: RegionalWhnfWork::from_focus(
                        access,
                        access.values().duplicate_value(results),
                    )
                    .with_source_owner(self.source_owner),
                    api: access.values().duplicate_value(api),
                };
                RegionalBuiltinPoll::Yielded
            }
            EffectPhase::MapResults {
                function,
                items,
                results,
                api,
            } => {
                let results = match poll_whnf_in(access, results, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                if !matches!(results, Value::List(_)) {
                    return failure("effect map internal results must be a list");
                }
                self.phase = EffectPhase::MapFront {
                    function: access.values().duplicate_value(function),
                    items: RegionalListFront::new_in(
                        access,
                        access.values().duplicate_value(items),
                        Some(self.source_owner),
                    ),
                    results,
                    api: access.values().duplicate_value(api),
                };
                RegionalBuiltinPoll::Yielded
            }
            EffectPhase::MapFront {
                function,
                items,
                results,
                api,
            } => match items.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(Some((item, tail))) => RegionalBuiltinPoll::Ready(
                    effect_map_sequence_in(access, function, &item, &tail, results, api),
                ),
                RegionalListFrontPoll::Ready(None) => RegionalBuiltinPoll::Ready(effect_call_in(
                    access,
                    api,
                    &keys::R,
                    std::slice::from_ref(results),
                )),
                RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalListFrontPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            },
            EffectPhase::MapContinue {
                function,
                items,
                results,
                result,
            } => match effect_map_continuation_in(access, function, items, results, result) {
                Ok(value) => RegionalBuiltinPoll::Ready(value),
                Err(failure) => RegionalBuiltinPoll::Failed(failure),
            },
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.phase.trace_managed_edges(visitor);
    }
}

impl EffectPhase {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match self {
            Self::Apply {
                function,
                argument,
                api,
            } => {
                function.trace_managed_edges(visitor);
                for value in [argument, api] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
            }
            Self::CallName {
                name,
                arguments,
                api,
            } => {
                name.trace_managed_edges(visitor);
                for value in [arguments, api] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
            }
            Self::CallArguments { arguments, api, .. } => {
                arguments.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(api, visitor);
            }
            Self::CallItems {
                items,
                arguments,
                api,
                ..
            } => {
                items.trace_managed_edges(visitor);
                for argument in arguments {
                    trace_compatibility_value_managed_edges(argument, visitor);
                }
                trace_compatibility_value_managed_edges(api, visitor);
            }
            Self::Fixpoint { function } => function.trace_managed_edges(visitor),
            Self::Map { function, items } => {
                for value in [function, items] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
            }
            Self::MapItems {
                function,
                items,
                results,
                api,
            } => {
                trace_compatibility_value_managed_edges(function, visitor);
                items.trace_managed_edges(visitor);
                for value in [results, api] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
            }
            Self::MapResults {
                function,
                items,
                results,
                api,
            } => {
                for value in [function, items, api] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
                results.trace_managed_edges(visitor);
            }
            Self::MapFront {
                function,
                items,
                results,
                api,
            } => {
                trace_compatibility_value_managed_edges(function, visitor);
                items.trace_managed_edges(visitor);
                for value in [results, api] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
            }
            Self::MapContinue {
                function,
                items,
                results,
                result,
            } => {
                for value in [function, items, results, result] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
            }
        }
    }
}

fn poll_whnf_in(
    access: &EvaluationValueAccess<'_>,
    computation: &mut RegionalWhnfWork,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Result<Value, RegionalBuiltinPoll> {
    match drive_regional_in_place(access, computation, step_budget, reduce_semantic_shell) {
        RegionalWhnfStatus::Ready(value) => Ok(value),
        RegionalWhnfStatus::Boundary(request) => Err(RegionalBuiltinPoll::Boundary(request)),
        RegionalWhnfStatus::Yielded => Err(RegionalBuiltinPoll::Yielded),
        RegionalWhnfStatus::Failed(failure) => Err(RegionalBuiltinPoll::Failed(failure)),
    }
}

fn application_in(
    access: &EvaluationValueAccess<'_>,
    function: Value,
    arguments: &[&Value],
) -> Value {
    let arguments = arguments
        .iter()
        .map(|argument| access.values().duplicate_value(argument))
        .collect::<Vec<_>>();
    Value::Lazy(LazyValue::from_application_in(
        access.values(),
        function,
        Arc::from(arguments),
    ))
}

fn effect_call_in(
    access: &EvaluationValueAccess<'_>,
    api: &Value,
    name: &Key,
    arguments: &[Value],
) -> Value {
    let selected = LazyValue::from_access_in(
        access.values(),
        Arc::from([CoreDataKey::Key(name.clone())]),
        Arc::from([access.values().duplicate_value(api)]),
    );
    let operation = if arguments.is_empty() {
        selected
    } else {
        let arguments = arguments
            .iter()
            .map(|argument| access.values().duplicate_value(argument))
            .collect::<Vec<_>>();
        LazyValue::from_application_in(access.values(), Value::Lazy(selected), Arc::from(arguments))
    };
    Value::Lazy(operation)
}

fn effect_map_sequence_in(
    access: &EvaluationValueAccess<'_>,
    function: &Value,
    item: &Value,
    tail: &Value,
    results: &Value,
    api: &Value,
) -> Value {
    let operation = Value::Lazy(LazyValue::from_application_in(
        access.values(),
        access.values().duplicate_value(function),
        Arc::from([access.values().duplicate_value(item)]),
    ));
    let continuation = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectMapContinue,
        arguments: Arc::from([
            access.values().duplicate_value(function),
            access.values().duplicate_value(tail),
            access.values().duplicate_value(results),
        ]),
    });
    let selected = LazyValue::from_access_in(
        access.values(),
        Arc::from([CoreDataKey::Key((*keys::SEQ).clone())]),
        Arc::from([access.values().duplicate_value(api)]),
    );
    Value::Lazy(LazyValue::from_application_in(
        access.values(),
        Value::Lazy(selected),
        Arc::from([operation, continuation]),
    ))
}

fn effect_map_in(access: &EvaluationValueAccess<'_>, function: &Value, items: &Value) -> Value {
    let function = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectMapRun,
        arguments: Arc::from([
            access.values().duplicate_value(function),
            access.values().duplicate_value(items),
            Value::List(List::empty()),
        ]),
    });
    super::application::effect_value(access.values(), function)
}

fn effect_map_continuation_in(
    access: &EvaluationValueAccess<'_>,
    function: &Value,
    items: &Value,
    results: &Value,
    result: &Value,
) -> Result<Value, Arc<EvaluationFailure>> {
    let Value::List(results) = results else {
        return Err(Arc::new(EvaluationFailure::message(
            "effect map internal results must be a list",
        )));
    };
    let results = List::concat(
        results.clone(),
        List::from_values(vec![access.values().duplicate_value(result)]),
    );
    let function = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectMapRun,
        arguments: Arc::from([
            access.values().duplicate_value(function),
            access.values().duplicate_value(items),
            Value::List(results),
        ]),
    });
    Ok(super::application::effect_value(access.values(), function))
}

fn failure(message: impl Into<Arc<str>>) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message.into())))
}
