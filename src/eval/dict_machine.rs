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
use super::application::predicate_effect;
use super::builtin_machine::RegionalBuiltinPoll;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll, item_value_in};
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
    /// `DictUpdate`, where `{}` removes, or `DictInsert`, where it is a value.
    Update {
        builtin: Builtin,
        path: Option<Box<RegionalKeyList>>,
        keys: Option<Vec<Key>>,
        new_value: Value,
        dict: RegionalWhnfWork,
    },
    Walk(Box<RegionalDictPathWalk>),
    ShallowUnion(RegionalSequentialDemands),
    /// `DictUpdateWith`'s composition, built at once.
    Composed(Value),
    MergeDuplicate(RegionalSequentialDemands),
    Pairs(RegionalWhnfWork),
    FromEntries(Box<RegionalDictFromEntries>),
}

/// A walk down a path, forcing each dict on the way and nothing else:
/// `DictLookup`, `DictDefines`, `DictDefault` and `DictRemove`
/// (`dict-presence-ops`).
struct RegionalDictPathWalk {
    builtin: Builtin,
    /// The path, until it is converted to `keys`.
    path: Option<Box<RegionalKeyList>>,
    keys: Vec<Key>,
    /// The dict operand, never forced where the path is empty.
    dict: Value,
    /// `DictDefault`'s fallback.
    fallback: Option<Value>,
    /// The dicts walked, the operand's first, each a key deeper.
    walked: Vec<Value>,
    /// The demand on the next dict on the way.
    demand: Option<RegionalWhnfWork>,
    source_owner: LazyId,
}

/// `dict_from_entries keys values`: the keys converted, then the values'
/// spine walked a strict leaf at a time, leaving each value as it is. For
/// `dict_from_pairs`, a walk of the pairs first finds the keys and values.
struct RegionalDictFromEntries {
    pairs: Option<Box<RegionalPairsWalk>>,
    keys: Option<Box<RegionalKeyList>>,
    converted: Vec<Key>,
    values: Option<Value>,
    values_demand: Option<RegionalWhnfWork>,
    front: Option<RegionalListFront>,
    items: Vec<Value>,
    source_owner: LazyId,
}

/// Walks a list of `[key, value]` lists, a pair per step, each to exactly
/// two items.
struct RegionalPairsWalk {
    list: Option<RegionalWhnfWork>,
    outer: Option<RegionalListFront>,
    pair_demand: Option<RegionalWhnfWork>,
    pair: Option<RegionalListFront>,
    pair_items: Vec<Value>,
    keys: Vec<Value>,
    values: Vec<Value>,
    source_owner: LazyId,
}

enum RegionalPairsPoll {
    Ready,
    Pending(RegionalBuiltinPoll),
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
    Continue,
    BudgetExhausted,
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
            Builtin::DictShallowUnion => RegionalDictBuiltinState::ShallowUnion(
                RegionalSequentialDemands::new_in(access, source_owner, arguments),
            ),
            Builtin::DictLookup
            | Builtin::DictDefines
            | Builtin::DictDefault
            | Builtin::DictRemove => {
                let (path, fallback, dict) = match arguments {
                    [path, dict] => (path, None, dict),
                    [path, fallback, dict] => (path, Some(fallback), dict),
                    _ => unreachable!("a path walk retains its path and dict"),
                };
                RegionalDictBuiltinState::Walk(Box::new(RegionalDictPathWalk {
                    builtin,
                    path: Some(Box::new(RegionalKeyList::new(
                        access,
                        access.values().duplicate_value(path),
                        Some(source_owner),
                    ))),
                    keys: Vec::new(),
                    dict: access.values().duplicate_value(dict),
                    fallback: fallback.map(|fallback| access.values().duplicate_value(fallback)),
                    walked: Vec::new(),
                    demand: None,
                    source_owner,
                }))
            }
            Builtin::DictUpdateWith => {
                let [path, function, dict] = arguments else {
                    unreachable!("dictionary update retains three operands")
                };
                let values = access.values();
                let current = Value::builtin_call_in(
                    values,
                    Builtin::DictLookup,
                    vec![values.duplicate_value(path), values.duplicate_value(dict)],
                );
                let updated = Value::Lazy(LazyValue::from_application_in(
                    values,
                    values.duplicate_value(function),
                    Arc::from([current]),
                ));
                RegionalDictBuiltinState::Composed(Value::builtin_call_in(
                    values,
                    Builtin::DictInsert,
                    vec![
                        values.duplicate_value(path),
                        updated,
                        values.duplicate_value(dict),
                    ],
                ))
            }
            Builtin::DictUpdate | Builtin::DictInsert => {
                let [path, new_value, dict] = arguments else {
                    unreachable!("dictionary update retains three operands")
                };
                RegionalDictBuiltinState::Update {
                    builtin,
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
            Builtin::DictPairs => {
                let [dict] = arguments else {
                    unreachable!("dictionary pairs retain one operand")
                };
                RegionalDictBuiltinState::Pairs(
                    RegionalWhnfWork::from_focus(access, access.values().duplicate_value(dict))
                        .with_source_owner(source_owner),
                )
            }
            Builtin::DictFromEntries => {
                let [keys, values] = arguments else {
                    unreachable!("dictionary from entries retains two operands")
                };
                RegionalDictBuiltinState::FromEntries(Box::new(RegionalDictFromEntries {
                    pairs: None,
                    keys: Some(Box::new(RegionalKeyList::new(
                        access,
                        access.values().duplicate_value(keys),
                        Some(source_owner),
                    ))),
                    converted: Vec::new(),
                    values: Some(access.values().duplicate_value(values)),
                    values_demand: None,
                    front: None,
                    items: Vec::new(),
                    source_owner,
                }))
            }
            Builtin::DictFromPairs => {
                let [pairs] = arguments else {
                    unreachable!("dictionary from pairs retains one operand")
                };
                RegionalDictBuiltinState::FromEntries(Box::new(RegionalDictFromEntries {
                    pairs: Some(Box::new(RegionalPairsWalk {
                        list: Some(
                            RegionalWhnfWork::from_focus(
                                access,
                                access.values().duplicate_value(pairs),
                            )
                            .with_source_owner(source_owner),
                        ),
                        outer: None,
                        pair_demand: None,
                        pair: None,
                        pair_items: Vec::new(),
                        keys: Vec::new(),
                        values: Vec::new(),
                        source_owner,
                    })),
                    keys: None,
                    converted: Vec::new(),
                    values: None,
                    values_demand: None,
                    front: None,
                    items: Vec::new(),
                    source_owner,
                }))
            }
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
            RegionalDictBuiltinState::Pairs(dict) => {
                let dict =
                    match drive_regional_in_place(access, dict, step_budget, reduce_semantic_shell)
                    {
                        RegionalWhnfStatus::Ready(value) => {
                            EvaluatedValue::from_whnf_in(access.values(), value)
                                .expect("dictionary demand must reach WHNF")
                                .into_value_in(access.values())
                        }
                        RegionalWhnfStatus::Boundary(request) => {
                            return RegionalBuiltinPoll::Boundary(request);
                        }
                        RegionalWhnfStatus::BudgetExhausted => {
                            return RegionalBuiltinPoll::BudgetExhausted;
                        }
                        RegionalWhnfStatus::Failed(failure) => {
                            return RegionalBuiltinPoll::Failed(failure);
                        }
                    };
                let Value::Dict(dict) = dict else {
                    return failure("dict pairs builtin requires a dictionary");
                };
                RegionalBuiltinPoll::Ready(Value::List(List::from_values(
                    dict.iter()
                        .map(|(key, value)| {
                            Value::List(List::from_values(vec![
                                access.values().key_value(key),
                                access.values().duplicate_value(value),
                            ]))
                        })
                        .collect(),
                )))
            }
            RegionalDictBuiltinState::FromEntries(entries) => entries.poll_in(access, step_budget),
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
                    RegionalConversionPoll::Continue => RegionalBuiltinPoll::Continue,
                    RegionalConversionPoll::BudgetExhausted => RegionalBuiltinPoll::BudgetExhausted,
                    RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
                }
            }
            RegionalDictBuiltinState::Union(demands) => {
                let operands = match demands.poll_in(access, step_budget) {
                    RegionalSequentialDemandPoll::Ready(operands) => operands,
                    RegionalSequentialDemandPoll::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalSequentialDemandPoll::Continue => {
                        return RegionalBuiltinPoll::Continue;
                    }
                    RegionalSequentialDemandPoll::BudgetExhausted => {
                        return RegionalBuiltinPoll::BudgetExhausted;
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
            RegionalDictBuiltinState::ShallowUnion(demands) => {
                let operands = match demands.poll_in(access, step_budget) {
                    RegionalSequentialDemandPoll::Ready(operands) => operands,
                    RegionalSequentialDemandPoll::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalSequentialDemandPoll::Continue => {
                        return RegionalBuiltinPoll::Continue;
                    }
                    RegionalSequentialDemandPoll::BudgetExhausted => {
                        return RegionalBuiltinPoll::BudgetExhausted;
                    }
                    RegionalSequentialDemandPoll::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
                let [left, right] = operands else {
                    unreachable!("dictionary union retains two operands")
                };
                finish_shallow_union_in(access, left, right)
            }
            RegionalDictBuiltinState::Walk(walk) => walk.poll_in(access, step_budget),
            RegionalDictBuiltinState::Composed(value) => {
                RegionalBuiltinPoll::Ready(access.values().duplicate_value(value))
            }
            RegionalDictBuiltinState::Update {
                builtin,
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
                            RegionalBuiltinPoll::Continue
                        }
                        RegionalConversionPoll::Boundary(request) => {
                            RegionalBuiltinPoll::Boundary(request)
                        }
                        RegionalConversionPoll::Continue => RegionalBuiltinPoll::Continue,
                        RegionalConversionPoll::BudgetExhausted => {
                            RegionalBuiltinPoll::BudgetExhausted
                        }
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
                        if *builtin == Builtin::DictInsert {
                            "dict.insert requires a non-empty path"
                        } else {
                            "dict update builtin requires a non-empty path"
                        },
                    )));
                }
                let dict =
                    match drive_regional_in_place(access, dict, step_budget, reduce_semantic_shell)
                    {
                        RegionalWhnfStatus::Ready(value) => {
                            EvaluatedValue::from_whnf_in(access.values(), value)
                                .expect("dictionary demand must reach WHNF")
                                .into_value_in(access.values())
                        }
                        RegionalWhnfStatus::Boundary(request) => {
                            return RegionalBuiltinPoll::Boundary(request);
                        }
                        RegionalWhnfStatus::BudgetExhausted => {
                            return RegionalBuiltinPoll::BudgetExhausted;
                        }
                        RegionalWhnfStatus::Failed(failure) => {
                            return RegionalBuiltinPoll::Failed(failure);
                        }
                    };
                if *builtin == Builtin::DictInsert {
                    finish_insert_in(access, path, new_value, &dict)
                } else {
                    finish_update_in(access, path, new_value, &dict)
                }
            }
            RegionalDictBuiltinState::MergeDuplicate(demands) => {
                let operands = match demands.poll_in(access, step_budget) {
                    RegionalSequentialDemandPoll::Ready(operands) => operands,
                    RegionalSequentialDemandPoll::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalSequentialDemandPoll::Continue => {
                        return RegionalBuiltinPoll::Continue;
                    }
                    RegionalSequentialDemandPoll::BudgetExhausted => {
                        return RegionalBuiltinPoll::BudgetExhausted;
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
            RegionalDictBuiltinState::Pairs(dict) => dict.trace_managed_edges(visitor),
            RegionalDictBuiltinState::FromEntries(entries) => entries.trace_managed_edges(visitor),
            RegionalDictBuiltinState::Singleton { key, value } => {
                key.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(value, visitor);
            }
            RegionalDictBuiltinState::Union(demands)
            | RegionalDictBuiltinState::ShallowUnion(demands)
            | RegionalDictBuiltinState::MergeDuplicate(demands) => {
                demands.trace_managed_edges(visitor);
            }
            RegionalDictBuiltinState::Walk(walk) => walk.trace_managed_edges(visitor),
            RegionalDictBuiltinState::Composed(value) => {
                trace_compatibility_value_managed_edges(value, visitor);
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

impl RegionalDictPathWalk {
    fn name(&self) -> &'static str {
        match self.builtin {
            Builtin::DictLookup => "dict.lookup",
            Builtin::DictDefines => "dict.defines",
            Builtin::DictDefault => "dict.default",
            Builtin::DictRemove => "dict.remove",
            _ => unreachable!("a path walk serves only its builtins"),
        }
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if let Some(machine) = &mut self.path {
            return match machine.poll_in(access, step_budget) {
                RegionalConversionPoll::Ready(keys) => {
                    self.path = None;
                    self.keys = keys;
                    if self.keys.is_empty() {
                        return self.finish_empty(access);
                    }
                    self.demand = Some(
                        RegionalWhnfWork::from_focus(
                            access,
                            access.values().duplicate_value(&self.dict),
                        )
                        .with_source_owner(self.source_owner),
                    );
                    RegionalBuiltinPoll::Continue
                }
                RegionalConversionPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalConversionPoll::Continue => RegionalBuiltinPoll::Continue,
                RegionalConversionPoll::BudgetExhausted => RegionalBuiltinPoll::BudgetExhausted,
                RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }
        let demand = self
            .demand
            .as_mut()
            .expect("a walk demands the next dict until it finishes");
        let value =
            match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                RegionalWhnfStatus::Ready(value) => {
                    EvaluatedValue::from_whnf_in(access.values(), value)
                        .expect("dictionary demand must reach WHNF")
                        .into_value_in(access.values())
                }
                RegionalWhnfStatus::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalWhnfStatus::BudgetExhausted => return RegionalBuiltinPoll::BudgetExhausted,
                RegionalWhnfStatus::Failed(failure) => return RegionalBuiltinPoll::Failed(failure),
            };
        self.demand = None;
        let depth = self.walked.len();
        let Value::Dict(dict) = &value else {
            return failure(&format!(
                "{} path `{}` traverses a non-dictionary value",
                self.name(),
                render_path(&self.keys[..depth])
            ));
        };
        let member = dict
            .get(&self.keys[depth])
            .map(|member| access.values().duplicate_value(member));
        self.walked.push(value);
        match member {
            None => self.finish_undefined(access),
            Some(member) if depth + 1 == self.keys.len() => self.finish_defined(access, member),
            Some(member) => {
                self.demand = Some(
                    RegionalWhnfWork::from_focus(access, member)
                        .with_source_owner(self.source_owner),
                );
                RegionalBuiltinPoll::Continue
            }
        }
    }

    /// The empty path names the dict itself, which is defined.
    fn finish_empty(&self, access: &EvaluationValueAccess<'_>) -> RegionalBuiltinPoll {
        match self.builtin {
            Builtin::DictLookup | Builtin::DictDefault => {
                RegionalBuiltinPoll::Ready(access.values().duplicate_value(&self.dict))
            }
            Builtin::DictDefines => {
                RegionalBuiltinPoll::Ready(predicate_effect(access.values(), true))
            }
            _ => failure("dict.remove requires a non-empty path"),
        }
    }

    fn finish_undefined(&self, access: &EvaluationValueAccess<'_>) -> RegionalBuiltinPoll {
        match self.builtin {
            Builtin::DictLookup => failure(&format!(
                "dict.lookup path `{}` is undefined",
                render_path(&self.keys)
            )),
            Builtin::DictDefines => {
                RegionalBuiltinPoll::Ready(predicate_effect(access.values(), false))
            }
            Builtin::DictDefault => RegionalBuiltinPoll::Ready(
                access.values().duplicate_value(
                    self.fallback
                        .as_ref()
                        .expect("a default retains its fallback"),
                ),
            ),
            _ => RegionalBuiltinPoll::Ready(access.values().duplicate_value(&self.walked[0])),
        }
    }

    fn finish_defined(
        &self,
        access: &EvaluationValueAccess<'_>,
        member: Value,
    ) -> RegionalBuiltinPoll {
        match self.builtin {
            Builtin::DictLookup | Builtin::DictDefault => RegionalBuiltinPoll::Ready(member),
            Builtin::DictDefines => {
                RegionalBuiltinPoll::Ready(predicate_effect(access.values(), true))
            }
            _ => {
                // Remove the leaf, then prune each dict on the way that the
                // removal left empty.
                let leaf = self.keys.len() - 1;
                let mut result = walked_dict(access, &self.walked[leaf]).remove(&self.keys[leaf]);
                for depth in (0..leaf).rev() {
                    let parent = walked_dict(access, &self.walked[depth]);
                    result = if result.is_empty() {
                        parent.remove(&self.keys[depth])
                    } else {
                        parent.insert(self.keys[depth].clone(), Value::Dict(result))
                    };
                }
                RegionalBuiltinPoll::Ready(Value::Dict(result))
            }
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(path) = &self.path {
            path.trace_managed_edges(visitor);
        }
        trace_compatibility_value_managed_edges(&self.dict, visitor);
        if let Some(fallback) = &self.fallback {
            trace_compatibility_value_managed_edges(fallback, visitor);
        }
        for value in &self.walked {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

fn walked_dict<'value>(_access: &EvaluationValueAccess<'_>, value: &'value Value) -> &'value Dict {
    let Value::Dict(dict) = value else {
        unreachable!("a walk keeps only the dicts on its way")
    };
    dict
}

fn render_path(keys: &[Key]) -> String {
    keys.iter()
        .map(format_name_part)
        .collect::<Vec<_>>()
        .join(".")
}

impl RegionalDictFromEntries {
    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if let Some(pairs) = &mut self.pairs {
            match pairs.poll_in(access, step_budget) {
                RegionalPairsPoll::Ready => {}
                RegionalPairsPoll::Pending(poll) => return poll,
            }
            let keys = Value::List(List::from_values(std::mem::take(&mut pairs.keys)));
            self.items = std::mem::take(&mut pairs.values);
            self.pairs = None;
            self.keys = Some(Box::new(RegionalKeyList::new(
                access,
                keys,
                Some(self.source_owner),
            )));
            return RegionalBuiltinPoll::Continue;
        }
        if let Some(keys) = &mut self.keys {
            return match keys.poll_in(access, step_budget) {
                RegionalConversionPoll::Ready(keys) => {
                    self.converted = keys;
                    self.keys = None;
                    RegionalBuiltinPoll::Continue
                }
                RegionalConversionPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalConversionPoll::Continue => RegionalBuiltinPoll::Continue,
                RegionalConversionPoll::BudgetExhausted => RegionalBuiltinPoll::BudgetExhausted,
                RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }
        if self.front.is_none() && (self.values.is_some() || self.values_demand.is_some()) {
            if let Some(values) = self.values.take() {
                self.values_demand = Some(
                    RegionalWhnfWork::from_focus(access, values)
                        .with_source_owner(self.source_owner),
                );
            }
            let demand = self
                .values_demand
                .as_mut()
                .expect("dictionary entries demand their values once");
            let values =
                match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(value) => {
                        EvaluatedValue::from_whnf_in(access.values(), value)
                            .expect("dictionary entry values must reach WHNF")
                            .into_value_in(access.values())
                    }
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::BudgetExhausted => {
                        return RegionalBuiltinPoll::BudgetExhausted;
                    }
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
            self.values_demand = None;
            let values = match values {
                Value::Binary(bytes) => Value::List(List::from_bytes(bytes)),
                values @ Value::List(_) => values,
                _ => return failure("dict from entries builtin requires a list of values"),
            };
            self.front = Some(RegionalListFront::new_in(
                access,
                values,
                Some(self.source_owner),
            ));
            return RegionalBuiltinPoll::Continue;
        }
        let Some(front) = &mut self.front else {
            // `dict_from_pairs` found its values with its keys.
            return self.finish_in(access);
        };
        loop {
            let (leaf, tail) = match front.poll_leaf_in(access, step_budget) {
                RegionalListFrontPoll::Ready(Some(step)) => step,
                RegionalListFrontPoll::Ready(None) => return self.finish_in(access),
                RegionalListFrontPoll::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalListFrontPoll::Continue => return RegionalBuiltinPoll::Continue,
                RegionalListFrontPoll::BudgetExhausted => {
                    return RegionalBuiltinPoll::BudgetExhausted;
                }
                RegionalListFrontPoll::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };
            let len = leaf.known_len().expect("a strict leaf knows its length");
            for index in 0..len {
                let item = leaf
                    .leaf_item_at_by(index, &mut |value| access.values().duplicate_value(value));
                self.items.push(item_value_in(access, item));
            }
            *front = RegionalListFront::new_in(access, tail, Some(self.source_owner));
        }
    }

    fn finish_in(&mut self, access: &EvaluationValueAccess<'_>) -> RegionalBuiltinPoll {
        if self.converted.len() != self.items.len() {
            return failure("dict from entries builtin requires as many values as keys");
        }
        let mut dict = Dict::new_sync();
        let mut seen = std::collections::BTreeSet::new();
        for (key, value) in self.converted.drain(..).zip(self.items.drain(..)) {
            if !seen.insert(key.clone()) {
                return failure("dict from entries builtin requires distinct keys");
            }
            if !is_undefined_dict_value(access.values(), &value) {
                dict = dict.insert(key, value);
            }
        }
        RegionalBuiltinPoll::Ready(Value::Dict(dict))
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(pairs) = &self.pairs {
            pairs.trace_managed_edges(visitor);
        }
        if let Some(keys) = &self.keys {
            keys.trace_managed_edges(visitor);
        }
        if let Some(values) = &self.values {
            trace_compatibility_value_managed_edges(values, visitor);
        }
        if let Some(demand) = &self.values_demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(front) = &self.front {
            front.trace_managed_edges(visitor);
        }
        for item in &self.items {
            trace_compatibility_value_managed_edges(item, visitor);
        }
    }
}

impl RegionalPairsWalk {
    /// Walks the pairs, a pair per step: `Ready` once each gave its key and
    /// value.
    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalPairsPoll {
        use RegionalPairsPoll::Pending;
        const TWO: &str = "dict from pairs builtin requires two items in each pair";
        if let Some(list) = &mut self.list {
            let list = match demand_list_in(
                access,
                list,
                step_budget,
                "dict from pairs builtin requires a list of pairs",
            ) {
                Ok(list) => list,
                Err(poll) => return Pending(poll),
            };
            self.list = None;
            self.outer = Some(RegionalListFront::new_in(
                access,
                list,
                Some(self.source_owner),
            ));
            return Pending(RegionalBuiltinPoll::Continue);
        }
        if let Some(demand) = &mut self.pair_demand {
            let pair = match demand_list_in(access, demand, step_budget, TWO) {
                Ok(pair) => pair,
                Err(poll) => return Pending(poll),
            };
            self.pair_demand = None;
            self.pair = Some(RegionalListFront::new_in(
                access,
                pair,
                Some(self.source_owner),
            ));
        }
        if let Some(pair) = &mut self.pair {
            loop {
                match pair.poll_in(access, step_budget) {
                    RegionalListFrontPoll::Ready(Some((item, tail))) => {
                        if self.pair_items.len() == 2 {
                            return Pending(failure(TWO));
                        }
                        self.pair_items.push(item);
                        *pair = RegionalListFront::new_in(access, tail, Some(self.source_owner));
                    }
                    RegionalListFrontPoll::Ready(None) => break,
                    poll => return Pending(front_pending(poll)),
                }
            }
            let mut items = std::mem::take(&mut self.pair_items);
            let (Some(value), Some(key), None) = (items.pop(), items.pop(), items.pop()) else {
                return Pending(failure(TWO));
            };
            self.keys.push(key);
            self.values.push(value);
            self.pair = None;
            return Pending(RegionalBuiltinPoll::Continue);
        }
        let outer = self.outer.as_mut().expect("the pairs walk their list");
        match outer.poll_in(access, step_budget) {
            RegionalListFrontPoll::Ready(Some((pair, tail))) => {
                *outer = RegionalListFront::new_in(access, tail, Some(self.source_owner));
                self.pair_demand = Some(
                    RegionalWhnfWork::from_focus(access, pair).with_source_owner(self.source_owner),
                );
                Pending(RegionalBuiltinPoll::Continue)
            }
            RegionalListFrontPoll::Ready(None) => RegionalPairsPoll::Ready,
            poll => Pending(front_pending(poll)),
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(list) = &self.list {
            list.trace_managed_edges(visitor);
        }
        if let Some(outer) = &self.outer {
            outer.trace_managed_edges(visitor);
        }
        if let Some(demand) = &self.pair_demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(pair) = &self.pair {
            pair.trace_managed_edges(visitor);
        }
        for value in self.pair_items.iter().chain(&self.keys).chain(&self.values) {
            trace_compatibility_value_managed_edges(value, visitor);
        }
    }
}

/// A list front's pause, passed up.
fn front_pending<T>(poll: RegionalListFrontPoll<T>) -> RegionalBuiltinPoll {
    match poll {
        RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
        RegionalListFrontPoll::Continue => RegionalBuiltinPoll::Continue,
        RegionalListFrontPoll::BudgetExhausted => RegionalBuiltinPoll::BudgetExhausted,
        RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
        RegionalListFrontPoll::Ready(_) => unreachable!("a list front's result is its caller's"),
    }
}

/// Demands `demand` to a list, a binary being the list of its bytes, or
/// fails with `message`.
fn demand_list_in(
    access: &EvaluationValueAccess<'_>,
    demand: &mut RegionalWhnfWork,
    step_budget: &mut EvaluationStepBudget,
    message: &str,
) -> Result<Value, RegionalBuiltinPoll> {
    match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
        RegionalWhnfStatus::Ready(value) => {
            match EvaluatedValue::from_whnf_in(access.values(), value)
                .expect("a list demand must reach WHNF")
                .into_value_in(access.values())
            {
                Value::Binary(bytes) => Ok(Value::List(List::from_bytes(bytes))),
                list @ Value::List(_) => Ok(list),
                _ => Err(failure(message)),
            }
        }
        RegionalWhnfStatus::Boundary(request) => Err(RegionalBuiltinPoll::Boundary(request)),
        RegionalWhnfStatus::BudgetExhausted => Err(RegionalBuiltinPoll::BudgetExhausted),
        RegionalWhnfStatus::Failed(failure) => Err(RegionalBuiltinPoll::Failed(failure)),
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
                    EvaluatedValue::from_whnf_in(access.values(), value)
                        .expect("sequential demand must reach WHNF")
                        .into_value_in(access.values()),
                );
                self.demand = None;
                RegionalSequentialDemandPoll::Continue
            }
            RegionalWhnfStatus::Boundary(request) => {
                RegionalSequentialDemandPoll::Boundary(request)
            }
            RegionalWhnfStatus::BudgetExhausted => RegionalSequentialDemandPoll::BudgetExhausted,
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

/// The shallow union: a key both dicts define is an error where it is read,
/// whatever its values (`dict-presence-ops`).
fn finish_shallow_union_in(
    access: &EvaluationValueAccess<'_>,
    left: &Value,
    right: &Value,
) -> RegionalBuiltinPoll {
    let (Value::Dict(left), Value::Dict(right)) = (left, right) else {
        return failure("dict.union requires dictionary values");
    };
    let (mut union, other) = if left.size() >= right.size() {
        (left.clone(), right)
    } else {
        (right.clone(), left)
    };
    for (key, value) in other.iter() {
        let value = if union.contains_key(key) {
            Value::Lazy(LazyValue::error_in(
                access.values(),
                format!("dict.union: both dicts define `{}`", format_name_part(key)),
            ))
        } else {
            access.values().duplicate_value(value)
        };
        union = union.insert(key.clone(), value);
    }
    RegionalBuiltinPoll::Ready(Value::Dict(union))
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

fn finish_insert_in(
    access: &EvaluationValueAccess<'_>,
    path: &[Key],
    new_value: &Value,
    dict: &Value,
) -> RegionalBuiltinPoll {
    let Value::Dict(dict) = dict else {
        return failure("dict.insert requires a dictionary");
    };
    RegionalBuiltinPoll::Ready(Value::Dict(insert_dict_path_in(
        access.values(),
        dict,
        path,
        access.values().duplicate_value(new_value),
    )))
}

/// `dict` with `new_value` at `path`, adding the dicts on the way where
/// absent. A lazy dict on the way takes the rest of the insert lazily, as
/// `update_dict_path_in` does, so nothing is forced that would not be.
fn insert_dict_path_in(
    access: &crate::core::RuntimeValueAccess<'_>,
    dict: &Dict,
    path: &[Key],
    new_value: Value,
) -> Dict {
    let mut parents = Vec::with_capacity(path.len().saturating_sub(1));
    let mut current = dict.clone();
    let mut index = 0;
    let next = loop {
        let head = &path[index];
        let rest = &path[index + 1..];
        if rest.is_empty() {
            break new_value;
        }
        match current.get(head).map(|value| access.duplicate_value(value)) {
            None => {
                parents.push((current, head.clone()));
                current = Dict::new_sync();
                index += 1;
            }
            Some(Value::Dict(nested)) => {
                parents.push((current, head.clone()));
                current = nested;
                index += 1;
            }
            Some(prior @ (Value::Lazy(_) | Value::Promised(_))) => {
                break builtin_apply3_value_in(
                    access,
                    Builtin::DictInsert,
                    &key_path_value(access, rest),
                    &new_value,
                    &prior,
                );
            }
            Some(_) => {
                break Value::Lazy(LazyValue::error_in(
                    access,
                    format!(
                        "dict.insert path `{}` traverses a non-dictionary value",
                        format_name_part(head)
                    ),
                ));
            }
        }
    };
    let mut result = current.insert(path[index].clone(), next);
    while let Some((parent, head)) = parents.pop() {
        result = parent.insert(head, Value::Dict(result));
    }
    result
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

fn render_name(access: &crate::core::RuntimeValueAccess<'_>, value: &Value) -> String {
    match value {
        Value::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Atom(atom) => match atom.key() {
            Key::Binary(bytes) => std::str::from_utf8(bytes)
                .map(str::to_owned)
                .unwrap_or_else(|_| format!("{atom:?}")),
            _ => format!("{atom:?}"),
        },
        other => format!("{:?}", access.diagnostic_debug(other)),
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
