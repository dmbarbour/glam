//! Resumable dictionary builtin operands and access-qualified transformations.

use std::collections::VecDeque;
use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluatedValue, EvaluationFailure, Key, LazyId, LazyValue, List,
    Value, trace_compatibility_value_managed_edges,
};
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};

use super::access_machine::{RegionalConversionPoll, RegionalKeyConversion, RegionalKeyList};
use super::builtin_machine::RegionalBuiltinPoll;
use super::value::{
    format_name_part, is_deferred_value, is_error_lazy_value, is_undefined_dict_value,
};
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalDictBuiltinMachine {
    state: RegionalDictBuiltinState,
}

enum RegionalDictBuiltinState {
    Singleton {
        key: Box<RegionalKeyConversion>,
        value: Value,
    },
    Union(RegionalSequentialDemands),
    Update {
        path: Option<Box<RegionalKeyList>>,
        keys: Option<Vec<Key>>,
        new_value: Value,
        dict: RegionalWhnfWork,
    },
    MergeDuplicate(RegionalSequentialDemands),
}

struct RegionalSequentialDemands {
    remaining: VecDeque<Value>,
    demand: Option<RegionalWhnfWork>,
    ready: Vec<Value>,
    source_owner: LazyId,
}

enum RegionalSequentialDemandPoll<'a> {
    Ready(&'a [Value]),
    Boundary(super::whnf::RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

impl RegionalDictBuiltinMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let state = match builtin {
            Builtin::DictSingleton => {
                let [key, value] = arguments else {
                    unreachable!("dictionary singleton retains two operands")
                };
                RegionalDictBuiltinState::Singleton {
                    key: Box::new(RegionalKeyConversion::new(
                        access,
                        access.values().duplicate_value(key),
                        Some(source_owner),
                    )),
                    value: access.values().duplicate_value(value),
                }
            }
            Builtin::DictUnion => RegionalDictBuiltinState::Union(
                RegionalSequentialDemands::new_in(access, source_owner, arguments),
            ),
            Builtin::DictUpdate => {
                let [path, new_value, dict] = arguments else {
                    unreachable!("dictionary update retains three operands")
                };
                RegionalDictBuiltinState::Update {
                    path: Some(Box::new(RegionalKeyList::new(
                        access,
                        access.values().duplicate_value(path),
                        Some(source_owner),
                    ))),
                    keys: None,
                    new_value: access.values().duplicate_value(new_value),
                    dict: RegionalWhnfWork::from_focus(
                        access,
                        access.values().duplicate_value(dict),
                    )
                    .with_source_owner(source_owner),
                }
            }
            Builtin::MergeDuplicate => RegionalDictBuiltinState::MergeDuplicate(
                RegionalSequentialDemands::new_in(access, source_owner, arguments),
            ),
            _ => unreachable!("dictionary machine received another builtin"),
        };
        Self { state }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match &mut self.state {
            RegionalDictBuiltinState::Singleton { key, value } => {
                match key.poll_in(access, step_budget) {
                    RegionalConversionPoll::Ready(key) => {
                        let value = access.values().duplicate_value(value);
                        let dict = if is_undefined_dict_value(access.values(), &value) {
                            Dict::new_sync()
                        } else {
                            Dict::new_sync().insert(key, value)
                        };
                        RegionalBuiltinPoll::Ready(Value::Dict(dict))
                    }
                    RegionalConversionPoll::Boundary(request) => {
                        RegionalBuiltinPoll::Boundary(request)
                    }
                    RegionalConversionPoll::Yielded => RegionalBuiltinPoll::Yielded,
                    RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
                }
            }
            RegionalDictBuiltinState::Union(demands) => {
                let operands = match demands.poll_in(access, step_budget) {
                    RegionalSequentialDemandPoll::Ready(operands) => operands,
                    RegionalSequentialDemandPoll::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalSequentialDemandPoll::Yielded => {
                        return RegionalBuiltinPoll::Yielded;
                    }
                    RegionalSequentialDemandPoll::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
                let [left, right] = operands else {
                    unreachable!("dictionary union retains two operands")
                };
                finish_union_in(access, left, right)
            }
            RegionalDictBuiltinState::Update {
                path,
                keys,
                new_value,
                dict,
            } => {
                if let Some(machine) = path {
                    return match machine.poll_in(access, step_budget) {
                        RegionalConversionPoll::Ready(path_keys) => {
                            *keys = Some(path_keys);
                            *path = None;
                            RegionalBuiltinPoll::Yielded
                        }
                        RegionalConversionPoll::Boundary(request) => {
                            RegionalBuiltinPoll::Boundary(request)
                        }
                        RegionalConversionPoll::Yielded => RegionalBuiltinPoll::Yielded,
                        RegionalConversionPoll::Failed(failure) => {
                            RegionalBuiltinPoll::Failed(failure)
                        }
                    };
                }
                let path = keys
                    .as_ref()
                    .expect("dictionary update path conversion must finish once");
                if path.is_empty() {
                    return RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(
                        "dict update builtin requires a non-empty path",
                    )));
                }
                let dict =
                    match drive_regional_in_place(access, dict, step_budget, reduce_semantic_shell)
                    {
                        RegionalWhnfStatus::Ready(value) => EvaluatedValue::try_from(value)
                            .expect("dictionary demand must reach WHNF")
                            .into_value(),
                        RegionalWhnfStatus::Boundary(request) => {
                            return RegionalBuiltinPoll::Boundary(request);
                        }
                        RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                        RegionalWhnfStatus::Failed(failure) => {
                            return RegionalBuiltinPoll::Failed(failure);
                        }
                    };
                finish_update_in(access, path, new_value, &dict)
            }
            RegionalDictBuiltinState::MergeDuplicate(demands) => {
                let operands = match demands.poll_in(access, step_budget) {
                    RegionalSequentialDemandPoll::Ready(operands) => operands,
                    RegionalSequentialDemandPoll::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalSequentialDemandPoll::Yielded => {
                        return RegionalBuiltinPoll::Yielded;
                    }
                    RegionalSequentialDemandPoll::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
                let [name, left, right] = operands else {
                    unreachable!("merge duplicate retains three operands")
                };
                finish_merge_duplicate_in(access, name, left, right)
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.state {
            RegionalDictBuiltinState::Singleton { key, value } => {
                key.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(value, visitor);
            }
            RegionalDictBuiltinState::Union(demands)
            | RegionalDictBuiltinState::MergeDuplicate(demands) => {
                demands.trace_managed_edges(visitor);
            }
            RegionalDictBuiltinState::Update {
                path,
                new_value,
                dict,
                ..
            } => {
                if let Some(path) = path {
                    path.trace_managed_edges(visitor);
                }
                trace_compatibility_value_managed_edges(new_value, visitor);
                dict.trace_managed_edges(visitor);
            }
        }
    }
}

impl RegionalSequentialDemands {
    fn new_in(access: &EvaluationValueAccess<'_>, source_owner: LazyId, values: &[Value]) -> Self {
        Self {
            remaining: values
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect(),
            demand: None,
            ready: Vec::new(),
            source_owner,
        }
    }

    fn poll_in<'a>(
        &'a mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalSequentialDemandPoll<'a> {
        if self.demand.is_none() {
            let Some(next) = self.remaining.pop_front() else {
                return RegionalSequentialDemandPoll::Ready(&self.ready);
            };
            self.demand = Some(
                RegionalWhnfWork::from_focus(access, next).with_source_owner(self.source_owner),
            );
        };
        match drive_regional_in_place(
            access,
            self.demand.as_mut().expect("demand was installed"),
            step_budget,
            reduce_semantic_shell,
        ) {
            RegionalWhnfStatus::Ready(value) => {
                self.ready.push(
                    EvaluatedValue::try_from(value)
                        .expect("sequential demand must reach WHNF")
                        .into_value(),
                );
                self.demand = None;
                RegionalSequentialDemandPoll::Yielded
            }
            RegionalWhnfStatus::Boundary(request) => {
                RegionalSequentialDemandPoll::Boundary(request)
            }
            RegionalWhnfStatus::Yielded => RegionalSequentialDemandPoll::Yielded,
            RegionalWhnfStatus::Failed(failure) => RegionalSequentialDemandPoll::Failed(failure),
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for value in &self.remaining {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
        for value in &self.ready {
            trace_compatibility_value_managed_edges(value, visitor);
        }
    }
}

fn finish_union_in(
    access: &EvaluationValueAccess<'_>,
    left: &Value,
    right: &Value,
) -> RegionalBuiltinPoll {
    let Value::Dict(left) = left else {
        return failure("dictionary union requires dictionary values");
    };
    let Value::Dict(right) = right else {
        return failure("dictionary union requires dictionary values");
    };
    RegionalBuiltinPoll::Ready(Value::Dict(merge_dicts_in(access.values(), left, right)))
}

fn failure(message: &str) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message)))
}

fn finish_update_in(
    access: &EvaluationValueAccess<'_>,
    path: &[Key],
    new_value: &Value,
    dict: &Value,
) -> RegionalBuiltinPoll {
    let Value::Dict(dict) = dict else {
        return failure("dict update builtin requires a dictionary");
    };
    RegionalBuiltinPoll::Ready(Value::Dict(update_dict_path_in(
        access.values(),
        dict,
        path,
        access.values().duplicate_value(new_value),
    )))
}

fn finish_merge_duplicate_in(
    access: &EvaluationValueAccess<'_>,
    name: &Value,
    left: &Value,
    right: &Value,
) -> RegionalBuiltinPoll {
    let name = render_name(access.values(), name);
    let result = if is_undefined_dict_value(access.values(), left) {
        access.values().duplicate_value(right)
    } else if is_undefined_dict_value(access.values(), right)
        || is_error_lazy_value(access.values(), left)
    {
        access.values().duplicate_value(left)
    } else if is_error_lazy_value(access.values(), right) {
        access.values().duplicate_value(right)
    } else if let (Value::Dict(left), Value::Dict(right)) = (left, right) {
        Value::Dict(merge_dicts_in(access.values(), left, right))
    } else {
        Value::Lazy(LazyValue::error_in(
            access.values(),
            format!("dictionary union is ambiguous at key `{name}`"),
        ))
    };
    RegionalBuiltinPoll::Ready(result)
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
    if path.is_empty() {
        return dict.clone();
    }

    let mut parents = Vec::with_capacity(path.len().saturating_sub(1));
    let mut current = dict.clone();
    let mut index = 0;
    let next = loop {
        let head = &path[index];
        let rest = &path[index + 1..];
        if rest.is_empty() {
            break new_value;
        }

        let prior = current
            .get(head)
            .map(|value| access.duplicate_value(value))
            .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
        match prior {
            Value::Dict(nested) => {
                parents.push((current, head.clone()));
                current = nested;
                index += 1;
            }
            Value::Lazy(_) | Value::Promised(_) => {
                break builtin_apply3_value_in(
                    access,
                    Builtin::DictUpdate,
                    &key_path_value(access, rest),
                    &new_value,
                    &prior,
                );
            }
            _ => {
                break Value::Lazy(LazyValue::error_in(
                    access,
                    format!(
                        "dictionary update path `{}` traverses a non-dictionary value",
                        format_name_part(head)
                    ),
                ));
            }
        }
    };

    let head = &path[index];
    let mut result = if is_undefined_dict_value(access, &next) {
        current.remove(head)
    } else {
        current.insert(head.clone(), next)
    };
    while let Some((parent, head)) = parents.pop() {
        let next = Value::Dict(result);
        result = if is_undefined_dict_value(access, &next) {
            parent.remove(&head)
        } else {
            parent.insert(head, next)
        };
    }
    result
}

fn key_path_value(access: &crate::core::RuntimeValueAccess<'_>, path: &[Key]) -> Value {
    Value::List(List::from_values(
        path.iter().map(|key| key_value(access, key)).collect(),
    ))
}

fn key_value(_access: &crate::core::RuntimeValueAccess<'_>, key: &Key) -> Value {
    enum Frame<'key> {
        List {
            items: &'key [Key],
            next: usize,
            values: Vec<Value>,
        },
        Dict {
            entries: &'key [(Key, Key)],
            next: usize,
            values: Dict,
        },
    }

    let mut frames = Vec::new();
    let mut current = key;
    loop {
        let mut value = match current {
            Key::Atom(atom) => Value::Atom(*atom),
            Key::Number(number) => Value::Number(number.clone()),
            Key::Binary(bytes) => Value::Binary(bytes.clone()),
            Key::AbstractGlobalPath(parts) => Value::Atom(crate::core::Atom::from_key(
                &Key::AbstractGlobalPath(parts.clone()),
            )),
            Key::List(items) if items.is_empty() => Value::List(List::empty()),
            Key::List(items) => {
                frames.push(Frame::List {
                    items,
                    next: 1,
                    values: Vec::with_capacity(items.len()),
                });
                current = &items[0];
                continue;
            }
            Key::Dict(entries) if entries.is_empty() => Value::Dict(Dict::new_sync()),
            Key::Dict(entries) => {
                frames.push(Frame::Dict {
                    entries,
                    next: 1,
                    values: Dict::new_sync(),
                });
                current = &entries[0].1;
                continue;
            }
        };

        loop {
            let Some(frame) = frames.pop() else {
                return value;
            };
            match frame {
                Frame::List {
                    items,
                    next,
                    mut values,
                } => {
                    values.push(value);
                    if next < items.len() {
                        frames.push(Frame::List {
                            items,
                            next: next + 1,
                            values,
                        });
                        current = &items[next];
                        break;
                    }
                    value = Value::List(List::from_values(values));
                }
                Frame::Dict {
                    entries,
                    next,
                    values,
                } => {
                    let values = values.insert(entries[next - 1].0.clone(), value);
                    if next < entries.len() {
                        frames.push(Frame::Dict {
                            entries,
                            next: next + 1,
                            values,
                        });
                        current = &entries[next].1;
                        break;
                    }
                    value = Value::Dict(values);
                }
            }
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
