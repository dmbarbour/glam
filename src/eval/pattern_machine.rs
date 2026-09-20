//! Durable compiler-pattern observations over list structure.

use std::sync::Arc;

use bytes::Bytes;
use glam_gc::Visitor;

use crate::core::{
    Atom, Builtin, BuiltinCall, Dict, Key, LazyId, List, RuntimeValueAccess, Value, keys,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};
use crate::number::Number;

use super::access_machine::{RegionalConversionPoll, RegionalKeyList};
use super::builtin_machine::RegionalBuiltinPoll;
use super::list_machine::{
    RegionalListBack, RegionalListBackPoll, RegionalListFront, RegionalListFrontPoll,
};
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

#[derive(Clone, Copy)]
enum PatternListOperation {
    IsList,
    TryUncons,
    TryUnsnoc,
    IsEmpty,
}

pub(in crate::eval) struct RegionalPatternListMachine {
    operation: PatternListOperation,
    source: Option<Value>,
    source_demand: Option<RegionalWhnfWork>,
    front: Option<RegionalListFront>,
    back: Option<RegionalListBack>,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalPatternPathMachine {
    expected: RegionalKeyList,
    expected_keys: Option<Vec<Key>>,
    actual: Option<Value>,
    actual_demand: Option<RegionalWhnfWork>,
    actual_keys: Option<RegionalKeyList>,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalPatternDictPredicateMachine {
    state: RegionalPatternDictPredicateState,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalPatternDictTakeMachine {
    optional: bool,
    path: RegionalKeyList,
    keys: Option<Vec<Key>>,
    source: Option<Value>,
    source_demand: Option<RegionalWhnfWork>,
    original: Option<Value>,
    current: Option<Value>,
    next_key: usize,
    selected: Option<RegionalWhnfWork>,
    selected_value: Option<Value>,
    frames: Vec<RegionalPatternDictTakeFrame>,
    undefined: Option<super::tagged_machine::RegionalSemanticUndefined>,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalPatternEqualMachine {
    expected: Option<Value>,
    expected_demand: Option<RegionalWhnfWork>,
    expected_literal: Option<PatternLiteral>,
    actual: Option<Value>,
    actual_demand: Option<RegionalWhnfWork>,
    list: Option<RegionalListFront>,
    item: Option<RegionalWhnfWork>,
    byte_index: usize,
    source_owner: LazyId,
}

enum PatternLiteral {
    Atom(Atom),
    Number(Number),
    Binary(Bytes),
    Unsupported(String),
}

struct RegionalPatternDictTakeFrame {
    parent: Value,
    key: Key,
}

enum PatternDictSelection {
    Missing,
    Present(Value),
}

enum RegionalPatternDictPredicateState {
    IsDict {
        source: Option<Value>,
        demand: Option<RegionalWhnfWork>,
    },
    IsEmpty(super::tagged_machine::RegionalSemanticUndefined),
}

impl RegionalPatternDictPredicateMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let [source] = arguments else {
            panic!("a dictionary pattern predicate retains one source")
        };
        let state = match builtin {
            Builtin::PatternIsDict => RegionalPatternDictPredicateState::IsDict {
                source: Some(access.values().duplicate_value(source)),
                demand: None,
            },
            Builtin::PatternDictIsEmpty => RegionalPatternDictPredicateState::IsEmpty(
                super::tagged_machine::RegionalSemanticUndefined::new_in(
                    access,
                    access.values().duplicate_value(source),
                    source_owner,
                ),
            ),
            _ => unreachable!("dictionary predicate machine received another builtin"),
        };
        Self {
            state,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match &mut self.state {
            RegionalPatternDictPredicateState::IsDict { source, demand } => {
                if demand.is_none() {
                    let source = source
                        .take()
                        .expect("dictionary kind pattern must retain its source");
                    *demand = Some(
                        RegionalWhnfWork::from_focus(access, source)
                            .with_source_owner(self.source_owner),
                    );
                }
                let source = match drive_regional_in_place(
                    access,
                    demand
                        .as_mut()
                        .expect("dictionary kind-pattern demand must be installed"),
                    step_budget,
                    reduce_semantic_shell,
                ) {
                    RegionalWhnfStatus::Ready(source) => source,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
                pattern_predicate_in(access, matches!(source, Value::Dict(_)))
            }
            RegionalPatternDictPredicateState::IsEmpty(undefined) => {
                match undefined.poll_in(access, step_budget) {
                    super::tagged_machine::RegionalSemanticUndefinedPoll::Ready(empty) => {
                        pattern_predicate_in(access, empty)
                    }
                    super::tagged_machine::RegionalSemanticUndefinedPoll::Boundary(request) => {
                        RegionalBuiltinPoll::Boundary(request)
                    }
                    super::tagged_machine::RegionalSemanticUndefinedPoll::Yielded => {
                        RegionalBuiltinPoll::Yielded
                    }
                    super::tagged_machine::RegionalSemanticUndefinedPoll::Failed(failure) => {
                        RegionalBuiltinPoll::Failed(failure)
                    }
                }
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.state {
            RegionalPatternDictPredicateState::IsDict { source, demand } => {
                if let Some(source) = source {
                    trace_compatibility_value_managed_edges(source, visitor);
                }
                if let Some(demand) = demand {
                    demand.trace_managed_edges(visitor);
                }
            }
            RegionalPatternDictPredicateState::IsEmpty(undefined) => {
                undefined.trace_managed_edges(visitor);
            }
        }
    }
}

impl RegionalPatternDictTakeMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let [path, source] = arguments else {
            panic!("dictionary pattern extraction retains two operands")
        };
        Self {
            optional: builtin == Builtin::PatternDictTryTakeOptional,
            path: RegionalKeyList::new(
                access,
                access.values().duplicate_value(path),
                Some(source_owner),
            ),
            keys: None,
            source: Some(access.values().duplicate_value(source)),
            source_demand: None,
            original: None,
            current: None,
            next_key: 0,
            selected: None,
            selected_value: None,
            frames: Vec::new(),
            undefined: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if self.keys.is_none() {
            return match self.path.poll_in(access, step_budget) {
                RegionalConversionPoll::Ready(keys) if keys.is_empty() => {
                    RegionalBuiltinPoll::Failed(Arc::new(crate::core::EvaluationFailure::message(
                        "pattern-dict-try-take received an empty compiler path",
                    )))
                }
                RegionalConversionPoll::Ready(keys) => {
                    self.keys = Some(keys);
                    RegionalBuiltinPoll::Yielded
                }
                RegionalConversionPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalConversionPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }

        if let Some(undefined) = &mut self.undefined {
            return match undefined.poll_in(access, step_budget) {
                super::tagged_machine::RegionalSemanticUndefinedPoll::Ready(true) => {
                    self.finish_absent(access)
                }
                super::tagged_machine::RegionalSemanticUndefinedPoll::Ready(false) => {
                    self.finish_found(access)
                }
                super::tagged_machine::RegionalSemanticUndefinedPoll::Boundary(request) => {
                    RegionalBuiltinPoll::Boundary(request)
                }
                super::tagged_machine::RegionalSemanticUndefinedPoll::Yielded => {
                    RegionalBuiltinPoll::Yielded
                }
                super::tagged_machine::RegionalSemanticUndefinedPoll::Failed(failure) => {
                    RegionalBuiltinPoll::Failed(failure)
                }
            };
        }

        if let Some(selected) = &mut self.selected {
            let value =
                match drive_regional_in_place(access, selected, step_budget, reduce_semantic_shell)
                {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
            self.selected = None;
            if self.next_key == self.keys().len() {
                self.undefined = Some(super::tagged_machine::RegionalSemanticUndefined::new_in(
                    access,
                    access.values().duplicate_value(&value),
                    self.source_owner,
                ));
                self.selected_value = Some(value);
                return RegionalBuiltinPoll::Yielded;
            }
            if !matches!(value, Value::Dict(_)) {
                return pattern_failure_in(access);
            }
            let key = self.keys()[self.next_key - 1].clone();
            self.frames.push(RegionalPatternDictTakeFrame {
                parent: self
                    .current
                    .as_ref()
                    .map(|parent| access.values().duplicate_value(parent))
                    .expect("dictionary extraction must retain its current parent"),
                key,
            });
            self.current = Some(value);
            return RegionalBuiltinPoll::Yielded;
        }

        if self.source.is_some() || self.source_demand.is_some() {
            if self.source_demand.is_none() {
                let source = self
                    .source
                    .take()
                    .expect("dictionary extraction must retain its source");
                self.source_demand = Some(
                    RegionalWhnfWork::from_focus(access, source)
                        .with_source_owner(self.source_owner),
                );
            }
            let source = match drive_regional_in_place(
                access,
                self.source_demand
                    .as_mut()
                    .expect("dictionary extraction source demand must be installed"),
                step_budget,
                reduce_semantic_shell,
            ) {
                RegionalWhnfStatus::Ready(source) => source,
                RegionalWhnfStatus::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalWhnfStatus::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };
            self.source_demand = None;
            if !matches!(source, Value::Dict(_)) {
                return pattern_failure_in(access);
            }
            self.original = Some(access.values().duplicate_value(&source));
            self.current = Some(source);
            return RegionalBuiltinPoll::Yielded;
        }

        let key = &self.keys()[self.next_key];
        let Value::Dict(dict) = self
            .current
            .as_ref()
            .expect("dictionary extraction must retain its current dictionary")
        else {
            unreachable!("dictionary extraction advances only through dictionaries")
        };
        let selection = dict
            .get(key)
            .map_or(PatternDictSelection::Missing, |value| {
                PatternDictSelection::Present(access.values().duplicate_value(value))
            });
        match selection {
            PatternDictSelection::Missing => self.finish_absent(access),
            PatternDictSelection::Present(value) => {
                self.next_key += 1;
                self.selected = Some(
                    RegionalWhnfWork::from_focus(access, value)
                        .with_source_owner(self.source_owner),
                );
                RegionalBuiltinPoll::Yielded
            }
        }
    }

    fn keys(&self) -> &[Key] {
        self.keys
            .as_deref()
            .expect("dictionary extraction path must be ready")
    }

    fn finish_absent(&self, access: &EvaluationValueAccess<'_>) -> RegionalBuiltinPoll {
        if !self.optional {
            return pattern_failure_in(access);
        }
        RegionalBuiltinPoll::Ready(pattern_success_value_in(
            access.values(),
            Value::Dict(
                Dict::new_sync()
                    .insert((*keys::VALUE).clone(), Value::Dict(Dict::new_sync()))
                    .insert(
                        (*keys::REST).clone(),
                        self.original
                            .as_ref()
                            .map(|original| access.values().duplicate_value(original))
                            .expect("optional extraction must retain its original dictionary"),
                    ),
            ),
        ))
    }

    fn finish_found(&self, access: &EvaluationValueAccess<'_>) -> RegionalBuiltinPoll {
        let Value::Dict(leaf_parent) = self
            .current
            .as_ref()
            .expect("successful extraction must retain its leaf parent")
        else {
            unreachable!("the extraction leaf parent must be a dictionary")
        };
        let mut rest = leaf_parent.remove(&self.keys()[self.next_key - 1]);
        for frame in self.frames.iter().rev() {
            let Value::Dict(parent) = &frame.parent else {
                unreachable!("an extraction frame parent must be a dictionary")
            };
            rest = if rest.is_empty() {
                parent.remove(&frame.key)
            } else {
                parent.insert(frame.key.clone(), Value::Dict(rest))
            };
        }
        RegionalBuiltinPoll::Ready(pattern_success_value_in(
            access.values(),
            Value::Dict(
                Dict::new_sync()
                    .insert(
                        (*keys::VALUE).clone(),
                        self.selected_value
                            .as_ref()
                            .map(|value| access.values().duplicate_value(value))
                            .expect("successful extraction must retain its selected value"),
                    )
                    .insert((*keys::REST).clone(), Value::Dict(rest)),
            ),
        ))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.path.trace_managed_edges(visitor);
        for value in [
            self.source.as_ref(),
            self.original.as_ref(),
            self.current.as_ref(),
            self.selected_value.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(source_demand) = &self.source_demand {
            source_demand.trace_managed_edges(visitor);
        }
        if let Some(selected) = &self.selected {
            selected.trace_managed_edges(visitor);
        }
        for frame in &self.frames {
            trace_compatibility_value_managed_edges(&frame.parent, visitor);
        }
        if let Some(undefined) = &self.undefined {
            undefined.trace_managed_edges(visitor);
        }
    }
}

impl RegionalPatternEqualMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let [expected, actual] = arguments else {
            panic!("pattern literal equality retains two operands")
        };
        Self {
            expected: Some(access.values().duplicate_value(expected)),
            expected_demand: None,
            expected_literal: None,
            actual: Some(access.values().duplicate_value(actual)),
            actual_demand: None,
            list: None,
            item: None,
            byte_index: 0,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if self.expected_literal.is_none() {
            if self.expected_demand.is_none() {
                let expected = self
                    .expected
                    .take()
                    .expect("pattern equality must retain its expected literal");
                self.expected_demand = Some(
                    RegionalWhnfWork::from_focus(access, expected)
                        .with_source_owner(self.source_owner),
                );
            }
            let expected = match drive_regional_in_place(
                access,
                self.expected_demand
                    .as_mut()
                    .expect("pattern literal demand must be installed"),
                step_budget,
                reduce_semantic_shell,
            ) {
                RegionalWhnfStatus::Ready(expected) => expected,
                RegionalWhnfStatus::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalWhnfStatus::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };
            self.expected_demand = None;
            self.expected_literal = Some(match expected {
                Value::Atom(atom) => PatternLiteral::Atom(atom),
                Value::Number(number) => PatternLiteral::Number(number),
                Value::Binary(bytes) => PatternLiteral::Binary(bytes),
                other => PatternLiteral::Unsupported(format!("{other:?}")),
            });
            return RegionalBuiltinPoll::Yielded;
        }

        if self.actual.is_some() || self.actual_demand.is_some() {
            if self.actual_demand.is_none() {
                let actual = self
                    .actual
                    .take()
                    .expect("pattern equality must retain its subject");
                self.actual_demand = Some(
                    RegionalWhnfWork::from_focus(access, actual)
                        .with_source_owner(self.source_owner),
                );
            }
            let actual = match drive_regional_in_place(
                access,
                self.actual_demand
                    .as_mut()
                    .expect("pattern subject demand must be installed"),
                step_budget,
                reduce_semantic_shell,
            ) {
                RegionalWhnfStatus::Ready(actual) => actual,
                RegionalWhnfStatus::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalWhnfStatus::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };
            self.actual_demand = None;
            self.actual = None;
            let literal = self
                .expected_literal
                .as_ref()
                .expect("pattern equality must retain its expected literal");
            if let PatternLiteral::Unsupported(expected) = literal {
                return RegionalBuiltinPoll::Failed(Arc::new(
                    crate::core::EvaluationFailure::message(format!(
                        "pattern-equal received unsupported compiler literal {expected}"
                    )),
                ));
            }
            let comparison = match (literal, actual) {
                (PatternLiteral::Atom(expected), Value::Atom(actual)) => {
                    PatternLiteralComparison::Ready(*expected == actual)
                }
                (PatternLiteral::Number(expected), Value::Number(actual)) => {
                    PatternLiteralComparison::Ready(expected == &actual)
                }
                (PatternLiteral::Binary(expected), Value::Binary(actual)) => {
                    PatternLiteralComparison::Ready(expected == &actual)
                }
                (PatternLiteral::Binary(_), actual @ Value::List(_)) => {
                    PatternLiteralComparison::List(actual)
                }
                (
                    PatternLiteral::Atom(_) | PatternLiteral::Number(_) | PatternLiteral::Binary(_),
                    _,
                ) => PatternLiteralComparison::Ready(false),
                (PatternLiteral::Unsupported(_), _) => {
                    unreachable!("unsupported literals are rejected before comparison")
                }
            };
            return match comparison {
                PatternLiteralComparison::Ready(equal) => pattern_predicate_in(access, equal),
                PatternLiteralComparison::List(actual) => {
                    self.list = Some(RegionalListFront::new_in(
                        access,
                        actual,
                        Some(self.source_owner),
                    ));
                    RegionalBuiltinPoll::Yielded
                }
            };
        }

        if let Some(item) = &mut self.item {
            let item =
                match drive_regional_in_place(access, item, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(item) => item,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
            self.item = None;
            let expected = self.expected_bytes()[self.byte_index];
            let matches = matches!(
                item,
                Value::Number(number) if number == Number::from_u8(expected)
            );
            if !matches {
                return pattern_failure_in(access);
            }
            self.byte_index += 1;
            return RegionalBuiltinPoll::Yielded;
        }

        let expected_len = self.expected_bytes().len();
        let list_poll = self
            .list
            .as_mut()
            .expect("binary/list pattern equality must retain list traversal")
            .poll_in(access, step_budget);
        match list_poll {
            RegionalListFrontPoll::Ready(None) => {
                pattern_predicate_in(access, self.byte_index == expected_len)
            }
            RegionalListFrontPoll::Ready(Some((_item, _tail)))
                if self.byte_index == expected_len =>
            {
                pattern_failure_in(access)
            }
            RegionalListFrontPoll::Ready(Some((item, tail))) => {
                self.list = Some(RegionalListFront::new_in(
                    access,
                    tail,
                    Some(self.source_owner),
                ));
                self.item = Some(
                    RegionalWhnfWork::from_focus(access, item).with_source_owner(self.source_owner),
                );
                RegionalBuiltinPoll::Yielded
            }
            RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
            RegionalListFrontPoll::Yielded => RegionalBuiltinPoll::Yielded,
            RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(expected) = &self.expected {
            trace_compatibility_value_managed_edges(expected, visitor);
        }
        if let Some(expected_demand) = &self.expected_demand {
            expected_demand.trace_managed_edges(visitor);
        }
        if let Some(actual) = &self.actual {
            trace_compatibility_value_managed_edges(actual, visitor);
        }
        if let Some(actual_demand) = &self.actual_demand {
            actual_demand.trace_managed_edges(visitor);
        }
        if let Some(list) = &self.list {
            list.trace_managed_edges(visitor);
        }
        if let Some(item) = &self.item {
            item.trace_managed_edges(visitor);
        }
    }

    fn expected_bytes(&self) -> &Bytes {
        let Some(PatternLiteral::Binary(bytes)) = &self.expected_literal else {
            unreachable!("list traversal is installed only for a binary literal")
        };
        bytes
    }
}

enum PatternLiteralComparison {
    Ready(bool),
    List(Value),
}

impl RegionalPatternPathMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let [expected, actual] = arguments else {
            panic!("pattern path equality retains two operands")
        };
        Self {
            expected: RegionalKeyList::new(
                access,
                access.values().duplicate_value(expected),
                Some(source_owner),
            ),
            expected_keys: None,
            actual: Some(access.values().duplicate_value(actual)),
            actual_demand: None,
            actual_keys: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if self.expected_keys.is_none() {
            return match self.expected.poll_in(access, step_budget) {
                RegionalConversionPoll::Ready(keys) => {
                    self.expected_keys = Some(keys);
                    RegionalBuiltinPoll::Yielded
                }
                RegionalConversionPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalConversionPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }

        if let Some(actual_keys) = &mut self.actual_keys {
            return match actual_keys.poll_optional_in(access, step_budget) {
                RegionalConversionPoll::Ready(Some(keys)) => {
                    pattern_predicate_in(access, self.expected_keys.as_ref() == Some(&keys))
                }
                RegionalConversionPoll::Ready(None) => pattern_failure_in(access),
                RegionalConversionPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalConversionPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }

        if self.actual_demand.is_none() {
            let actual = self
                .actual
                .take()
                .expect("pattern path subject must remain available until demanded");
            self.actual_demand = Some(
                RegionalWhnfWork::from_focus(access, actual).with_source_owner(self.source_owner),
            );
        }
        let actual = match drive_regional_in_place(
            access,
            self.actual_demand
                .as_mut()
                .expect("pattern path subject demand must be installed"),
            step_budget,
            reduce_semantic_shell,
        ) {
            RegionalWhnfStatus::Ready(actual) => actual,
            RegionalWhnfStatus::Boundary(request) => {
                return RegionalBuiltinPoll::Boundary(request);
            }
            RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
            RegionalWhnfStatus::Failed(failure) => {
                return RegionalBuiltinPoll::Failed(failure);
            }
        };
        self.actual_demand = None;
        let shape = match actual {
            Value::Binary(bytes) => PatternPathShape::Binary(bytes),
            actual @ Value::List(_) => PatternPathShape::List(actual),
            _ => PatternPathShape::Other,
        };
        match shape {
            PatternPathShape::Binary(bytes) => {
                let keys = bytes
                    .iter()
                    .map(|byte| Key::Number(Number::from_u8(*byte)))
                    .collect::<Vec<_>>();
                pattern_predicate_in(access, self.expected_keys.as_ref() == Some(&keys))
            }
            PatternPathShape::List(actual) => {
                self.actual_keys = Some(RegionalKeyList::from_ready(
                    access,
                    actual,
                    Some(self.source_owner),
                ));
                RegionalBuiltinPoll::Yielded
            }
            PatternPathShape::Other => pattern_failure_in(access),
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.expected.trace_managed_edges(visitor);
        if let Some(actual) = &self.actual {
            trace_compatibility_value_managed_edges(actual, visitor);
        }
        if let Some(actual) = &self.actual_demand {
            actual.trace_managed_edges(visitor);
        }
        if let Some(actual) = &self.actual_keys {
            actual.trace_managed_edges(visitor);
        }
    }
}

enum PatternPathShape {
    Binary(Bytes),
    List(Value),
    Other,
}

impl RegionalPatternListMachine {
    pub(in crate::eval) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::PatternIsList
                | Builtin::PatternListTryUncons
                | Builtin::PatternListTryUnsnoc
                | Builtin::PatternListIsEmpty
        )
    }

    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let operation = match builtin {
            Builtin::PatternIsList => PatternListOperation::IsList,
            Builtin::PatternListTryUncons => PatternListOperation::TryUncons,
            Builtin::PatternListTryUnsnoc => PatternListOperation::TryUnsnoc,
            Builtin::PatternListIsEmpty => PatternListOperation::IsEmpty,
            _ => unreachable!("pattern-list machine received another builtin"),
        };
        let [source] = arguments else {
            panic!("a pattern-list observation retains one source")
        };
        Self {
            operation,
            source: Some(access.values().duplicate_value(source)),
            source_demand: None,
            front: None,
            back: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if let Some(front) = &mut self.front {
            return match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(item) => self.finish_front(access, item),
                RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalListFrontPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }
        if let Some(back) = &mut self.back {
            return match back.poll_in(access, step_budget) {
                RegionalListBackPoll::Ready(item) => self.finish_back(access, item),
                RegionalListBackPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
                RegionalListBackPoll::Yielded => RegionalBuiltinPoll::Yielded,
                RegionalListBackPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
            };
        }

        if self.source_demand.is_none() {
            let source = self
                .source
                .take()
                .expect("pattern-list observation must retain its source");
            self.source_demand = Some(
                RegionalWhnfWork::from_focus(access, source).with_source_owner(self.source_owner),
            );
        }
        let source = match drive_regional_in_place(
            access,
            self.source_demand
                .as_mut()
                .expect("pattern-list source demand must be installed"),
            step_budget,
            reduce_semantic_shell,
        ) {
            RegionalWhnfStatus::Ready(source) => source,
            RegionalWhnfStatus::Boundary(request) => {
                return RegionalBuiltinPoll::Boundary(request);
            }
            RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
            RegionalWhnfStatus::Failed(failure) => return RegionalBuiltinPoll::Failed(failure),
        };
        self.source_demand = None;
        match source {
            Value::Binary(bytes) => self.finish_binary(access, bytes),
            source @ Value::List(_) => match self.operation {
                PatternListOperation::IsList => pattern_predicate_in(access, true),
                PatternListOperation::TryUncons | PatternListOperation::IsEmpty => {
                    self.front = Some(RegionalListFront::new_in(
                        access,
                        source,
                        Some(self.source_owner),
                    ));
                    RegionalBuiltinPoll::Yielded
                }
                PatternListOperation::TryUnsnoc => {
                    self.back = Some(RegionalListBack::new_in(
                        access,
                        source,
                        Some(self.source_owner),
                    ));
                    RegionalBuiltinPoll::Yielded
                }
            },
            _ => pattern_failure_in(access),
        }
    }

    fn finish_binary(
        &self,
        access: &EvaluationValueAccess<'_>,
        bytes: Bytes,
    ) -> RegionalBuiltinPoll {
        match self.operation {
            PatternListOperation::IsList => pattern_predicate_in(access, true),
            PatternListOperation::IsEmpty => pattern_predicate_in(access, bytes.is_empty()),
            PatternListOperation::TryUncons => match bytes.first() {
                Some(byte) => RegionalBuiltinPoll::Ready(pattern_success_value_in(
                    access.values(),
                    Value::Dict(
                        Dict::new_sync()
                            .insert((*keys::HEAD).clone(), Value::Number(Number::from_u8(*byte)))
                            .insert(
                                (*keys::TAIL).clone(),
                                Value::Binary(bytes.slice(1..bytes.len())),
                            ),
                    ),
                )),
                None => pattern_failure_in(access),
            },
            PatternListOperation::TryUnsnoc => match bytes.last() {
                Some(byte) => RegionalBuiltinPoll::Ready(pattern_success_value_in(
                    access.values(),
                    Value::Dict(
                        Dict::new_sync()
                            .insert(
                                (*keys::INIT).clone(),
                                Value::Binary(bytes.slice(0..bytes.len() - 1)),
                            )
                            .insert((*keys::LAST).clone(), Value::Number(Number::from_u8(*byte))),
                    ),
                )),
                None => pattern_failure_in(access),
            },
        }
    }

    fn finish_front(
        &self,
        access: &EvaluationValueAccess<'_>,
        item: Option<(Value, Value)>,
    ) -> RegionalBuiltinPoll {
        match self.operation {
            PatternListOperation::IsEmpty => pattern_predicate_in(access, item.is_none()),
            PatternListOperation::TryUncons => match item {
                Some((head, tail)) => RegionalBuiltinPoll::Ready(pattern_success_value_in(
                    access.values(),
                    Value::Dict(
                        Dict::new_sync()
                            .insert((*keys::HEAD).clone(), head)
                            .insert((*keys::TAIL).clone(), tail),
                    ),
                )),
                None => pattern_failure_in(access),
            },
            PatternListOperation::IsList | PatternListOperation::TryUnsnoc => {
                unreachable!("this pattern-list operation does not use front traversal")
            }
        }
    }

    fn finish_back(
        &self,
        access: &EvaluationValueAccess<'_>,
        item: Option<(Value, Value)>,
    ) -> RegionalBuiltinPoll {
        match item {
            Some((init, last)) => RegionalBuiltinPoll::Ready(pattern_success_value_in(
                access.values(),
                Value::Dict(
                    Dict::new_sync()
                        .insert((*keys::INIT).clone(), init)
                        .insert((*keys::LAST).clone(), last),
                ),
            )),
            None => pattern_failure_in(access),
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(source) = &self.source {
            trace_compatibility_value_managed_edges(source, visitor);
        }
        if let Some(demand) = &self.source_demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(front) = &self.front {
            front.trace_managed_edges(visitor);
        }
        if let Some(back) = &self.back {
            back.trace_managed_edges(visitor);
        }
    }
}

fn pattern_predicate_in(access: &EvaluationValueAccess<'_>, passes: bool) -> RegionalBuiltinPoll {
    if passes {
        RegionalBuiltinPoll::Ready(pattern_success_value_in(
            access.values(),
            Value::Atom(Atom::from_key(&keys::UNIT)),
        ))
    } else {
        pattern_failure_in(access)
    }
}

fn pattern_failure_in(access: &EvaluationValueAccess<'_>) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Ready(pattern_effect_value_in(
        access.values(),
        &keys::FAIL,
        vec![],
    ))
}

fn pattern_success_value_in(access: &RuntimeValueAccess<'_>, value: Value) -> Value {
    pattern_effect_value_in(access, &keys::R, vec![value])
}

fn pattern_effect_value_in(
    access: &RuntimeValueAccess<'_>,
    name: &crate::core::Key,
    arguments: Vec<Value>,
) -> Value {
    let crate::core::Key::Atom(name) = name else {
        unreachable!("standard effect request names are atom keys")
    };
    super::application::effect_value(
        access,
        Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::EffectCall,
            arguments: Arc::from([
                Value::Atom(*name),
                Value::List(List::from_values(arguments)),
            ]),
        }),
    )
}
