//! Resumable dictionary builtin operands and access-qualified transformations.

use std::sync::Arc;

use crate::core::{Builtin, BuiltinCall, Dict, EvaluationFailure, Key, LazyValue, List, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::access_machine::{ConversionPoll, KeyConversionMachine, KeyListMachine};
use super::builtin_machine::BuiltinTaskPoll;
use super::value::{
    format_name_part, is_deferred_value, is_error_lazy_value, is_undefined_dict_value,
};
use super::whnf::WhnfComputation;

pub(crate) struct DictBuiltinMachine {
    state: DictBuiltinState,
}

enum DictBuiltinState {
    Singleton {
        key: KeyConversionMachine,
        value: RuntimeValueRoot,
    },
    Union(SequentialDemands),
    Update {
        path: Option<Box<KeyListMachine>>,
        keys: Option<Vec<Key>>,
        new_value: RuntimeValueRoot,
        dict: WhnfComputation,
    },
    MergeDuplicate(SequentialDemands),
}

struct SequentialDemands {
    demands: Vec<WhnfComputation>,
    next: usize,
    ready: Vec<RuntimeValueRoot>,
}

enum SequentialDemandPoll<'a> {
    Ready(&'a [RuntimeValueRoot]),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl DictBuiltinMachine {
    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let state = match builtin {
            Builtin::DictSingleton => {
                let [key, value]: [RuntimeValueRoot; 2] = arguments
                    .try_into()
                    .expect("dictionary singleton retains two operands");
                DictBuiltinState::Singleton {
                    key: KeyConversionMachine::new(key, None),
                    value,
                }
            }
            Builtin::DictUnion => DictBuiltinState::Union(SequentialDemands::new(arguments)),
            Builtin::DictUpdate => {
                let [path, new_value, dict]: [RuntimeValueRoot; 3] = arguments
                    .try_into()
                    .expect("dictionary update retains three operands");
                DictBuiltinState::Update {
                    path: Some(Box::new(KeyListMachine::unowned(path))),
                    keys: None,
                    new_value,
                    dict: WhnfComputation::from_root(dict),
                }
            }
            Builtin::MergeDuplicate => {
                DictBuiltinState::MergeDuplicate(SequentialDemands::new(arguments))
            }
            _ => unreachable!("dictionary machine received another builtin"),
        };
        Self { state }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        match &mut self.state {
            DictBuiltinState::Singleton { key, value } => {
                match key.poll(poll_context, context, durable_context, step_budget) {
                    ConversionPoll::Ready(key) => {
                        let result = context.with_value_access(|access| {
                            let value = access.clone_root(value);
                            let dict = if is_undefined_dict_value(access.values(), &value) {
                                Dict::new_sync()
                            } else {
                                Dict::new_sync().insert(key, value)
                            };
                            access.values().root_runtime_value(Value::Dict(dict))
                        });
                        BuiltinTaskPoll::Ready(result)
                    }
                    ConversionPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                    ConversionPoll::Yielded => BuiltinTaskPoll::Yielded,
                    ConversionPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
                }
            }
            DictBuiltinState::Union(demands) => {
                let operands = match demands.poll(poll_context, durable_context, step_budget) {
                    SequentialDemandPoll::Ready(operands) => operands,
                    SequentialDemandPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    SequentialDemandPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    SequentialDemandPoll::Failed(failure) => {
                        return BuiltinTaskPoll::Failed(failure);
                    }
                };
                let [left, right] = operands else {
                    unreachable!("dictionary union retains two operands")
                };
                finish_union(context, left, right)
            }
            DictBuiltinState::Update {
                path,
                keys,
                new_value,
                dict,
            } => {
                if let Some(machine) = path {
                    return match machine.poll(poll_context, context, durable_context, step_budget) {
                        ConversionPoll::Ready(path_keys) => {
                            *keys = Some(path_keys);
                            *path = None;
                            BuiltinTaskPoll::Yielded
                        }
                        ConversionPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                        ConversionPoll::Yielded => BuiltinTaskPoll::Yielded,
                        ConversionPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
                    };
                }
                let path = keys
                    .as_ref()
                    .expect("dictionary update path conversion must finish once");
                if path.is_empty() {
                    return BuiltinTaskPoll::Failed(root_message(
                        context,
                        "dict update builtin requires a non-empty path",
                    ));
                }
                let dict = match poll_demand(dict, poll_context, durable_context, step_budget) {
                    DemandPoll::Ready(value) => value,
                    DemandPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    DemandPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    DemandPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
                };
                finish_update(context, path, new_value, &dict)
            }
            DictBuiltinState::MergeDuplicate(demands) => {
                let operands = match demands.poll(poll_context, durable_context, step_budget) {
                    SequentialDemandPoll::Ready(operands) => operands,
                    SequentialDemandPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    SequentialDemandPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    SequentialDemandPoll::Failed(failure) => {
                        return BuiltinTaskPoll::Failed(failure);
                    }
                };
                let [name, left, right] = operands else {
                    unreachable!("merge duplicate retains three operands")
                };
                finish_merge_duplicate(context, name, left, right)
            }
        }
    }
}

impl SequentialDemands {
    fn new(values: Vec<RuntimeValueRoot>) -> Self {
        Self {
            demands: values.into_iter().map(WhnfComputation::from_root).collect(),
            next: 0,
            ready: Vec::new(),
        }
    }

    fn poll<'a>(
        &'a mut self,
        poll_context: &EvaluationPollContext,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> SequentialDemandPoll<'a> {
        let Some(demand) = self.demands.get_mut(self.next) else {
            return SequentialDemandPoll::Ready(&self.ready);
        };
        match poll_demand(demand, poll_context, durable_context, step_budget) {
            DemandPoll::Ready(value) => {
                self.ready.push(value);
                self.next += 1;
                SequentialDemandPoll::Yielded
            }
            DemandPoll::Pending(dependency) => SequentialDemandPoll::Pending(dependency),
            DemandPoll::Yielded => SequentialDemandPoll::Yielded,
            DemandPoll::Failed(failure) => SequentialDemandPoll::Failed(failure),
        }
    }
}

enum DemandPoll {
    Ready(RuntimeValueRoot),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

fn poll_demand(
    demand: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    durable_context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> DemandPoll {
    match poll_whnf_computation(demand, poll_context, durable_context, step_budget) {
        WhnfOwnerPoll::Ready(value) => DemandPoll::Ready(value),
        WhnfOwnerPoll::Pending(dependency) => DemandPoll::Pending(dependency),
        WhnfOwnerPoll::Yielded => DemandPoll::Yielded,
        WhnfOwnerPoll::Failed(failure) => DemandPoll::Failed(failure),
        WhnfOwnerPoll::External(boundary) => {
            unreachable!("dictionary demand produced an external {boundary:?} boundary")
        }
    }
}

fn finish_union(
    context: &EvaluatorStepContext<'_>,
    left: &RuntimeValueRoot,
    right: &RuntimeValueRoot,
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        let Value::Dict(left) = access.clone_root(left) else {
            return Err("dictionary union requires dictionary values");
        };
        let Value::Dict(right) = access.clone_root(right) else {
            return Err("dictionary union requires dictionary values");
        };
        Ok(access
            .values()
            .root_runtime_value(Value::Dict(merge_dicts_in(access.values(), &left, &right))))
    });
    match result {
        Ok(value) => BuiltinTaskPoll::Ready(value),
        Err(message) => BuiltinTaskPoll::Failed(root_message(context, message)),
    }
}

fn finish_update(
    context: &EvaluatorStepContext<'_>,
    path: &[Key],
    new_value: &RuntimeValueRoot,
    dict: &RuntimeValueRoot,
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        let Value::Dict(dict) = access.clone_root(dict) else {
            return Err("dict update builtin requires a dictionary");
        };
        let new_value = access.clone_root(new_value);
        Ok(access
            .values()
            .root_runtime_value(Value::Dict(update_dict_path_in(
                access.values(),
                &dict,
                path,
                new_value,
            ))))
    });
    match result {
        Ok(value) => BuiltinTaskPoll::Ready(value),
        Err(message) => BuiltinTaskPoll::Failed(root_message(context, message)),
    }
}

fn finish_merge_duplicate(
    context: &EvaluatorStepContext<'_>,
    name: &RuntimeValueRoot,
    left: &RuntimeValueRoot,
    right: &RuntimeValueRoot,
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        let name = render_name(access.values(), &access.clone_root(name));
        let left = access.clone_root(left);
        let right = access.clone_root(right);
        let result = if is_undefined_dict_value(access.values(), &left) {
            right
        } else if is_undefined_dict_value(access.values(), &right)
            || is_error_lazy_value(access.values(), &left)
        {
            left
        } else if is_error_lazy_value(access.values(), &right) {
            right
        } else if let (Value::Dict(left), Value::Dict(right)) = (&left, &right) {
            Value::Dict(merge_dicts_in(access.values(), left, right))
        } else {
            Value::Lazy(LazyValue::error_in(
                access.values(),
                format!("dictionary union is ambiguous at key `{name}`"),
            ))
        };
        access.values().root_runtime_value(result)
    });
    BuiltinTaskPoll::Ready(result)
}

fn render_name(_access: &crate::core::RuntimeValueAccess<'_>, value: &Value) -> String {
    match value {
        Value::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Atom(atom) => match atom.key() {
            Key::Binary(bytes) => std::str::from_utf8(bytes)
                .map(str::to_owned)
                .unwrap_or_else(|_| format!("{atom:?}")),
            _ => format!("{atom:?}"),
        },
        other => format!("{other:?}"),
    }
}

pub(in crate::eval) fn merge_dicts_in(
    access: &crate::core::RuntimeValueAccess<'_>,
    left: &Dict,
    right: &Dict,
) -> Dict {
    let (mut merged, updates) = if left.size() >= right.size() {
        (left.clone(), right)
    } else {
        (right.clone(), left)
    };

    for (key, value) in updates.iter() {
        let next = match merged.get(key) {
            Some(existing) => Some(merge_duplicate_value_in(access, key, existing, value)),
            None if is_undefined_dict_value(access, value) => None,
            None => Some(access.duplicate_value(value)),
        };
        merged = match next {
            Some(value) if is_undefined_dict_value(access, &value) => merged.remove(key),
            Some(value) => merged.insert(key.clone(), value),
            None => merged,
        };
    }
    merged
}

fn merge_duplicate_value_in(
    access: &crate::core::RuntimeValueAccess<'_>,
    key: &Key,
    left: &Value,
    right: &Value,
) -> Value {
    if is_undefined_dict_value(access, left) {
        access.duplicate_value(right)
    } else if is_undefined_dict_value(access, right) {
        access.duplicate_value(left)
    } else if matches!((left, right), (Value::Dict(_), Value::Dict(_)))
        || is_deferred_value(access, left)
        || is_deferred_value(access, right)
    {
        builtin_apply3_value_in(
            access,
            Builtin::MergeDuplicate,
            &Value::binary_from_text(&format_name_part(key)),
            left,
            right,
        )
    } else {
        Value::Lazy(LazyValue::error_in(
            access,
            format!(
                "dictionary union is ambiguous at key `{}`",
                format_name_part(key)
            ),
        ))
    }
}

fn update_dict_path_in(
    access: &crate::core::RuntimeValueAccess<'_>,
    dict: &Dict,
    path: &[Key],
    new_value: Value,
) -> Dict {
    let Some((head, rest)) = path.split_first() else {
        return dict.clone();
    };
    let next = if rest.is_empty() {
        new_value
    } else {
        let prior = dict
            .get(head)
            .map(|value| access.duplicate_value(value))
            .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
        match prior {
            Value::Dict(dict) => Value::Dict(update_dict_path_in(access, &dict, rest, new_value)),
            Value::Lazy(_) | Value::Promised(_) => builtin_apply3_value_in(
                access,
                Builtin::DictUpdate,
                &key_path_value(access, rest),
                &new_value,
                &prior,
            ),
            _ => Value::Lazy(LazyValue::error_in(
                access,
                format!(
                    "dictionary update path `{}` traverses a non-dictionary value",
                    format_name_part(head)
                ),
            )),
        }
    };
    if is_undefined_dict_value(access, &next) {
        dict.remove(head)
    } else {
        dict.insert(head.clone(), next)
    }
}

fn key_path_value(access: &crate::core::RuntimeValueAccess<'_>, path: &[Key]) -> Value {
    Value::List(List::from_values(
        path.iter().map(|key| key_value(access, key)).collect(),
    ))
}

fn key_value(_access: &crate::core::RuntimeValueAccess<'_>, key: &Key) -> Value {
    match key {
        Key::Atom(atom) => Value::Atom(*atom),
        Key::Number(number) => Value::Number(number.clone()),
        Key::Binary(bytes) => Value::Binary(bytes.clone()),
        Key::AbstractGlobalPath(parts) => Value::Atom(crate::core::Atom::from_key(
            &Key::AbstractGlobalPath(parts.clone()),
        )),
        Key::List(items) => Value::List(List::from_values(
            items.iter().map(|key| key_value(_access, key)).collect(),
        )),
        Key::Dict(entries) => {
            Value::Dict(entries.iter().fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key.clone(), key_value(_access, value))
            }))
        }
    }
}

fn builtin_apply3_value_in(
    access: &crate::core::RuntimeValueAccess<'_>,
    builtin: Builtin,
    first: &Value,
    second: &Value,
    third: &Value,
) -> Value {
    Value::Lazy(LazyValue::from_builtin_in(
        access,
        BuiltinCall {
            builtin,
            arguments: Arc::from([
                access.duplicate_value(first),
                access.duplicate_value(second),
                access.duplicate_value(third),
            ]),
        },
    ))
}

fn root_message(context: &EvaluatorStepContext<'_>, message: &str) -> RuntimeFailureRoot {
    context.root_failure(Arc::new(EvaluationFailure::message(message)))
}
