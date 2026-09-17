//! Durable compiler-pattern observations over list structure.

use std::sync::Arc;

use bytes::Bytes;

use crate::core::{Atom, Builtin, BuiltinCall, Dict, Key, List, RuntimeValueAccess, Value, keys};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::number::Number;
use crate::runtime::RuntimeValueRoot;

use super::access_machine::{ConversionPoll, KeyListMachine};
use super::builtin_machine::BuiltinTaskPoll;
use super::list_machine::{ListBackMachine, ListBackPoll, ListFrontMachine, ListFrontPoll};
use super::tagged_machine::{SemanticUndefinedMachine, SemanticUndefinedPoll};
use super::whnf::WhnfComputation;

#[derive(Clone, Copy)]
enum PatternListOperation {
    IsList,
    TryUncons,
    TryUnsnoc,
    IsEmpty,
}

pub(crate) struct PatternListMachine {
    operation: PatternListOperation,
    source: WhnfComputation,
    front: Option<ListFrontMachine>,
    back: Option<ListBackMachine>,
}

pub(crate) struct PatternPathMachine {
    expected: KeyListMachine,
    expected_keys: Option<Vec<Key>>,
    actual: Option<WhnfComputation>,
    actual_keys: Option<KeyListMachine>,
}

pub(crate) struct PatternDictPredicateMachine {
    state: PatternDictPredicateState,
}

pub(crate) struct PatternDictTakeMachine {
    optional: bool,
    path: Option<KeyListMachine>,
    keys: Option<Vec<Key>>,
    source: Option<WhnfComputation>,
    original: Option<RuntimeValueRoot>,
    current: Option<RuntimeValueRoot>,
    next_key: usize,
    selected: Option<WhnfComputation>,
    selected_value: Option<RuntimeValueRoot>,
    frames: Vec<PatternDictTakeFrame>,
    undefined: Option<SemanticUndefinedMachine>,
}

pub(crate) struct PatternEqualMachine {
    expected: Option<WhnfComputation>,
    expected_literal: Option<PatternLiteral>,
    actual: Option<WhnfComputation>,
    list: Option<ListFrontMachine>,
    item: Option<WhnfComputation>,
    byte_index: usize,
}

enum PatternLiteral {
    Atom(Atom),
    Number(Number),
    Binary(Bytes),
    Unsupported(String),
}

struct PatternDictTakeFrame {
    parent: RuntimeValueRoot,
    key: Key,
}

enum PatternDictSelection {
    Missing,
    Present(RuntimeValueRoot),
}

enum PatternDictPredicateState {
    IsDict(WhnfComputation),
    IsEmpty(SemanticUndefinedMachine),
}

impl PatternDictPredicateMachine {
    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let [source]: [RuntimeValueRoot; 1] = arguments
            .try_into()
            .expect("a dictionary pattern predicate retains one source");
        let state = match builtin {
            Builtin::PatternIsDict => {
                PatternDictPredicateState::IsDict(WhnfComputation::from_root(source))
            }
            Builtin::PatternDictIsEmpty => {
                PatternDictPredicateState::IsEmpty(SemanticUndefinedMachine::new(source))
            }
            _ => unreachable!("dictionary predicate machine received another builtin"),
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
            PatternDictPredicateState::IsDict(source) => {
                let source = match poll_whnf_computation(
                    source,
                    poll_context,
                    durable_context,
                    step_budget,
                ) {
                    WhnfOwnerPoll::Ready(source) => source,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => {
                        return BuiltinTaskPoll::Failed(failure);
                    }
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!(
                            "dictionary pattern predicate produced an external {boundary:?} boundary"
                        )
                    }
                };
                let is_dict = context.with_value_access(|access| {
                    matches!(access.clone_root(&source), Value::Dict(_))
                });
                rooted_pattern_predicate(context, is_dict)
            }
            PatternDictPredicateState::IsEmpty(undefined) => {
                match undefined.poll(poll_context, context, durable_context, step_budget) {
                    SemanticUndefinedPoll::Ready(empty) => rooted_pattern_predicate(context, empty),
                    SemanticUndefinedPoll::Pending(dependency) => {
                        BuiltinTaskPoll::Pending(dependency)
                    }
                    SemanticUndefinedPoll::Yielded => BuiltinTaskPoll::Yielded,
                    SemanticUndefinedPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
                }
            }
        }
    }
}

impl PatternDictTakeMachine {
    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let [path, source]: [RuntimeValueRoot; 2] = arguments
            .try_into()
            .expect("dictionary pattern extraction retains two operands");
        Self {
            optional: builtin == Builtin::PatternDictTryTakeOptional,
            path: Some(KeyListMachine::unowned(path)),
            keys: None,
            source: Some(WhnfComputation::from_root(source)),
            original: None,
            current: None,
            next_key: 0,
            selected: None,
            selected_value: None,
            frames: Vec::new(),
            undefined: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if let Some(path) = &mut self.path {
            return match path.poll(poll_context, context, durable_context, step_budget) {
                ConversionPoll::Ready(keys) if keys.is_empty() => BuiltinTaskPoll::Failed(
                    context.root_failure(Arc::new(crate::core::EvaluationFailure::message(
                        "pattern-dict-try-take received an empty compiler path",
                    ))),
                ),
                ConversionPoll::Ready(keys) => {
                    self.keys = Some(keys);
                    self.path = None;
                    BuiltinTaskPoll::Yielded
                }
                ConversionPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ConversionPoll::Yielded => BuiltinTaskPoll::Yielded,
                ConversionPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            };
        }

        if let Some(undefined) = &mut self.undefined {
            return match undefined.poll(poll_context, context, durable_context, step_budget) {
                SemanticUndefinedPoll::Ready(true) => self.finish_absent(context),
                SemanticUndefinedPoll::Ready(false) => self.finish_found(context),
                SemanticUndefinedPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                SemanticUndefinedPoll::Yielded => BuiltinTaskPoll::Yielded,
                SemanticUndefinedPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            };
        }

        if let Some(selected) = &mut self.selected {
            let value =
                match poll_whnf_computation(selected, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(value) => value,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!(
                            "dictionary pattern member produced an external {boundary:?} boundary"
                        )
                    }
                };
            self.selected = None;
            if self.next_key == self.keys().len() {
                self.selected_value = Some(value.clone());
                self.undefined = Some(SemanticUndefinedMachine::new(value));
                return BuiltinTaskPoll::Yielded;
            }
            let is_dict = context
                .with_value_access(|access| matches!(access.clone_root(&value), Value::Dict(_)));
            if !is_dict {
                return rooted_pattern_failure(context);
            }
            let key = self.keys()[self.next_key - 1].clone();
            self.frames.push(PatternDictTakeFrame {
                parent: self
                    .current
                    .as_ref()
                    .expect("dictionary extraction must retain its current parent")
                    .clone(),
                key,
            });
            self.current = Some(value);
            return BuiltinTaskPoll::Yielded;
        }

        if let Some(source) = &mut self.source {
            let source =
                match poll_whnf_computation(source, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(source) => source,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!(
                            "dictionary pattern source produced an external {boundary:?} boundary"
                        )
                    }
                };
            self.source = None;
            let is_dict = context
                .with_value_access(|access| matches!(access.clone_root(&source), Value::Dict(_)));
            if !is_dict {
                return rooted_pattern_failure(context);
            }
            self.original = Some(source.clone());
            self.current = Some(source);
            return BuiltinTaskPoll::Yielded;
        }

        let key = &self.keys()[self.next_key];
        let selection = context.with_value_access(|access| {
            let Value::Dict(dict) = access.clone_root(
                self.current
                    .as_ref()
                    .expect("dictionary extraction must retain its current dictionary"),
            ) else {
                unreachable!("dictionary extraction advances only through dictionaries")
            };
            dict.get(key)
                .map_or(PatternDictSelection::Missing, |value| {
                    PatternDictSelection::Present(
                        access
                            .values()
                            .root_runtime_value(access.values().duplicate_value(value)),
                    )
                })
        });
        match selection {
            PatternDictSelection::Missing => self.finish_absent(context),
            PatternDictSelection::Present(value) => {
                self.next_key += 1;
                self.selected = Some(WhnfComputation::from_root(value));
                BuiltinTaskPoll::Yielded
            }
        }
    }

    fn keys(&self) -> &[Key] {
        self.keys
            .as_deref()
            .expect("dictionary extraction path must be ready")
    }

    fn finish_absent(&self, context: &EvaluatorStepContext<'_>) -> BuiltinTaskPoll {
        if !self.optional {
            return rooted_pattern_failure(context);
        }
        context.with_value_access(|access| {
            BuiltinTaskPoll::Ready(pattern_success_in(
                access.values(),
                Value::Dict(
                    Dict::new_sync()
                        .insert((*keys::VALUE).clone(), Value::Dict(Dict::new_sync()))
                        .insert(
                            (*keys::REST).clone(),
                            access.clone_root(
                                self.original.as_ref().expect(
                                    "optional extraction must retain its original dictionary",
                                ),
                            ),
                        ),
                ),
            ))
        })
    }

    fn finish_found(&self, context: &EvaluatorStepContext<'_>) -> BuiltinTaskPoll {
        context.with_value_access(|access| {
            let leaf_parent = self
                .current
                .as_ref()
                .expect("successful extraction must retain its leaf parent");
            let Value::Dict(leaf_parent) = access.clone_root(leaf_parent) else {
                unreachable!("the extraction leaf parent must be a dictionary")
            };
            let mut rest = leaf_parent.remove(&self.keys()[self.next_key - 1]);
            for frame in self.frames.iter().rev() {
                let Value::Dict(parent) = access.clone_root(&frame.parent) else {
                    unreachable!("an extraction frame parent must be a dictionary")
                };
                rest = if rest.is_empty() {
                    parent.remove(&frame.key)
                } else {
                    parent.insert(frame.key.clone(), Value::Dict(rest))
                };
            }
            BuiltinTaskPoll::Ready(pattern_success_in(
                access.values(),
                Value::Dict(
                    Dict::new_sync()
                        .insert(
                            (*keys::VALUE).clone(),
                            access.clone_root(
                                self.selected_value
                                    .as_ref()
                                    .expect("successful extraction must retain its selected value"),
                            ),
                        )
                        .insert((*keys::REST).clone(), Value::Dict(rest)),
                ),
            ))
        })
    }
}

impl PatternEqualMachine {
    pub(crate) fn new(arguments: Vec<RuntimeValueRoot>) -> Self {
        let [expected, actual]: [RuntimeValueRoot; 2] = arguments
            .try_into()
            .expect("pattern literal equality retains two operands");
        Self {
            expected: Some(WhnfComputation::from_root(expected)),
            expected_literal: None,
            actual: Some(WhnfComputation::from_root(actual)),
            list: None,
            item: None,
            byte_index: 0,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if let Some(expected) = &mut self.expected {
            let expected =
                match poll_whnf_computation(expected, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(expected) => expected,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("pattern literal produced an external {boundary:?} boundary")
                    }
                };
            self.expected = None;
            self.expected_literal =
                Some(
                    context.with_value_access(|access| match access.clone_root(&expected) {
                        Value::Atom(atom) => PatternLiteral::Atom(atom),
                        Value::Number(number) => PatternLiteral::Number(number),
                        Value::Binary(bytes) => PatternLiteral::Binary(bytes),
                        other => PatternLiteral::Unsupported(format!("{other:?}")),
                    }),
                );
            return BuiltinTaskPoll::Yielded;
        }

        if let Some(actual) = &mut self.actual {
            let actual =
                match poll_whnf_computation(actual, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(actual) => actual,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("pattern subject produced an external {boundary:?} boundary")
                    }
                };
            self.actual = None;
            let literal = self
                .expected_literal
                .as_ref()
                .expect("pattern equality must retain its expected literal");
            if let PatternLiteral::Unsupported(expected) = literal {
                return BuiltinTaskPoll::Failed(context.root_failure(Arc::new(
                    crate::core::EvaluationFailure::message(format!(
                        "pattern-equal received unsupported compiler literal {expected}"
                    )),
                )));
            }
            let comparison = context.with_value_access(|access| {
                let actual_value = access.clone_root(&actual);
                match (literal, actual_value) {
                    (PatternLiteral::Atom(expected), Value::Atom(actual)) => {
                        PatternLiteralComparison::Ready(*expected == actual)
                    }
                    (PatternLiteral::Number(expected), Value::Number(actual)) => {
                        PatternLiteralComparison::Ready(expected == &actual)
                    }
                    (PatternLiteral::Binary(expected), Value::Binary(actual)) => {
                        PatternLiteralComparison::Ready(expected == &actual)
                    }
                    (PatternLiteral::Binary(_), Value::List(_)) => PatternLiteralComparison::List,
                    (
                        PatternLiteral::Atom(_)
                        | PatternLiteral::Number(_)
                        | PatternLiteral::Binary(_),
                        _,
                    ) => PatternLiteralComparison::Ready(false),
                    (PatternLiteral::Unsupported(_), _) => {
                        unreachable!("unsupported literals are rejected before comparison")
                    }
                }
            });
            return match comparison {
                PatternLiteralComparison::Ready(equal) => rooted_pattern_predicate(context, equal),
                PatternLiteralComparison::List => {
                    self.list = Some(ListFrontMachine::unowned(actual));
                    BuiltinTaskPoll::Yielded
                }
            };
        }

        if let Some(item) = &mut self.item {
            let item = match poll_whnf_computation(item, poll_context, durable_context, step_budget)
            {
                WhnfOwnerPoll::Ready(item) => item,
                WhnfOwnerPoll::Pending(dependency) => {
                    return BuiltinTaskPoll::Pending(dependency);
                }
                WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
                WhnfOwnerPoll::External(boundary) => {
                    unreachable!("pattern list item produced an external {boundary:?} boundary")
                }
            };
            self.item = None;
            let expected = self.expected_bytes()[self.byte_index];
            let matches = context.with_value_access(|access| {
                matches!(
                    access.clone_root(&item),
                    Value::Number(number) if number == Number::from_u8(expected)
                )
            });
            if !matches {
                return rooted_pattern_failure(context);
            }
            self.byte_index += 1;
            return BuiltinTaskPoll::Yielded;
        }

        let expected_len = self.expected_bytes().len();
        let list_poll = self
            .list
            .as_mut()
            .expect("binary/list pattern equality must retain list traversal")
            .poll(poll_context, context, durable_context, step_budget);
        match list_poll {
            ListFrontPoll::Ready(None) => {
                rooted_pattern_predicate(context, self.byte_index == expected_len)
            }
            ListFrontPoll::Ready(Some((_item, _tail))) if self.byte_index == expected_len => {
                rooted_pattern_failure(context)
            }
            ListFrontPoll::Ready(Some((item, tail))) => {
                self.list = Some(ListFrontMachine::unowned(tail));
                self.item = Some(WhnfComputation::from_root(item));
                BuiltinTaskPoll::Yielded
            }
            ListFrontPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
            ListFrontPoll::Yielded => BuiltinTaskPoll::Yielded,
            ListFrontPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
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
    List,
}

impl PatternPathMachine {
    pub(crate) fn new(arguments: Vec<RuntimeValueRoot>) -> Self {
        let [expected, actual]: [RuntimeValueRoot; 2] = arguments
            .try_into()
            .expect("pattern path equality retains two operands");
        Self {
            expected: KeyListMachine::unowned(expected),
            expected_keys: None,
            actual: Some(WhnfComputation::from_root(actual)),
            actual_keys: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if self.expected_keys.is_none() {
            return match self
                .expected
                .poll(poll_context, context, durable_context, step_budget)
            {
                ConversionPoll::Ready(keys) => {
                    self.expected_keys = Some(keys);
                    BuiltinTaskPoll::Yielded
                }
                ConversionPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ConversionPoll::Yielded => BuiltinTaskPoll::Yielded,
                ConversionPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            };
        }

        if let Some(actual_keys) = &mut self.actual_keys {
            return match actual_keys.poll_optional(
                poll_context,
                context,
                durable_context,
                step_budget,
            ) {
                ConversionPoll::Ready(Some(keys)) => {
                    rooted_pattern_predicate(context, self.expected_keys.as_ref() == Some(&keys))
                }
                ConversionPoll::Ready(None) => rooted_pattern_failure(context),
                ConversionPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ConversionPoll::Yielded => BuiltinTaskPoll::Yielded,
                ConversionPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            };
        }

        let actual = match poll_whnf_computation(
            self.actual
                .as_mut()
                .expect("pattern path subject must retain demand until ready"),
            poll_context,
            durable_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(actual) => actual,
            WhnfOwnerPoll::Pending(dependency) => {
                return BuiltinTaskPoll::Pending(dependency);
            }
            WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("pattern path subject produced an external {boundary:?} boundary")
            }
        };
        self.actual = None;
        let shape = context.with_value_access(|access| match access.clone_root(&actual) {
            Value::Binary(bytes) => PatternPathShape::Binary(bytes),
            Value::List(_) => PatternPathShape::List,
            _ => PatternPathShape::Other,
        });
        match shape {
            PatternPathShape::Binary(bytes) => {
                let keys = bytes
                    .iter()
                    .map(|byte| Key::Number(Number::from_u8(*byte)))
                    .collect::<Vec<_>>();
                rooted_pattern_predicate(context, self.expected_keys.as_ref() == Some(&keys))
            }
            PatternPathShape::List => {
                self.actual_keys = Some(KeyListMachine::from_ready_unowned(actual));
                BuiltinTaskPoll::Yielded
            }
            PatternPathShape::Other => rooted_pattern_failure(context),
        }
    }
}

enum PatternPathShape {
    Binary(Bytes),
    List,
    Other,
}

impl PatternListMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::PatternIsList
                | Builtin::PatternListTryUncons
                | Builtin::PatternListTryUnsnoc
                | Builtin::PatternListIsEmpty
        )
    }

    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let operation = match builtin {
            Builtin::PatternIsList => PatternListOperation::IsList,
            Builtin::PatternListTryUncons => PatternListOperation::TryUncons,
            Builtin::PatternListTryUnsnoc => PatternListOperation::TryUnsnoc,
            Builtin::PatternListIsEmpty => PatternListOperation::IsEmpty,
            _ => unreachable!("pattern-list machine received another builtin"),
        };
        let [source]: [RuntimeValueRoot; 1] = arguments
            .try_into()
            .expect("a pattern-list observation retains one source");
        Self {
            operation,
            source: WhnfComputation::from_root(source),
            front: None,
            back: None,
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if let Some(front) = &mut self.front {
            return match front.poll(poll_context, context, durable_context, step_budget) {
                ListFrontPoll::Ready(item) => self.finish_front(context, item),
                ListFrontPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ListFrontPoll::Yielded => BuiltinTaskPoll::Yielded,
                ListFrontPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            };
        }
        if let Some(back) = &mut self.back {
            return match back.poll(poll_context, context, durable_context, step_budget) {
                ListBackPoll::Ready(item) => self.finish_back(context, item),
                ListBackPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                ListBackPoll::Yielded => BuiltinTaskPoll::Yielded,
                ListBackPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
            };
        }

        let source = match poll_whnf_computation(
            &mut self.source,
            poll_context,
            durable_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(source) => source,
            WhnfOwnerPoll::Pending(dependency) => {
                return BuiltinTaskPoll::Pending(dependency);
            }
            WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("pattern-list source produced an external {boundary:?} boundary")
            }
        };
        let shape = context.with_value_access(|access| match access.clone_root(&source) {
            Value::Binary(bytes) => PatternListShape::Binary(bytes),
            Value::List(_) => PatternListShape::List,
            _ => PatternListShape::Other,
        });
        match shape {
            PatternListShape::Binary(bytes) => self.finish_binary(context, bytes),
            PatternListShape::List => match self.operation {
                PatternListOperation::IsList => rooted_pattern_predicate(context, true),
                PatternListOperation::TryUncons | PatternListOperation::IsEmpty => {
                    self.front = Some(ListFrontMachine::unowned(source));
                    BuiltinTaskPoll::Yielded
                }
                PatternListOperation::TryUnsnoc => {
                    self.back = Some(ListBackMachine::new(source));
                    BuiltinTaskPoll::Yielded
                }
            },
            PatternListShape::Other => rooted_pattern_failure(context),
        }
    }

    fn finish_binary(&self, context: &EvaluatorStepContext<'_>, bytes: Bytes) -> BuiltinTaskPoll {
        match self.operation {
            PatternListOperation::IsList => rooted_pattern_predicate(context, true),
            PatternListOperation::IsEmpty => rooted_pattern_predicate(context, bytes.is_empty()),
            PatternListOperation::TryUncons => match bytes.first() {
                Some(byte) => context.with_value_access(|access| {
                    BuiltinTaskPoll::Ready(pattern_success_in(
                        access.values(),
                        Value::Dict(
                            Dict::new_sync()
                                .insert(
                                    (*keys::HEAD).clone(),
                                    Value::Number(Number::from_u8(*byte)),
                                )
                                .insert(
                                    (*keys::TAIL).clone(),
                                    Value::Binary(bytes.slice(1..bytes.len())),
                                ),
                        ),
                    ))
                }),
                None => rooted_pattern_failure(context),
            },
            PatternListOperation::TryUnsnoc => match bytes.last() {
                Some(byte) => context.with_value_access(|access| {
                    BuiltinTaskPoll::Ready(pattern_success_in(
                        access.values(),
                        Value::Dict(
                            Dict::new_sync()
                                .insert(
                                    (*keys::INIT).clone(),
                                    Value::Binary(bytes.slice(0..bytes.len() - 1)),
                                )
                                .insert(
                                    (*keys::LAST).clone(),
                                    Value::Number(Number::from_u8(*byte)),
                                ),
                        ),
                    ))
                }),
                None => rooted_pattern_failure(context),
            },
        }
    }

    fn finish_front(
        &self,
        context: &EvaluatorStepContext<'_>,
        item: Option<(RuntimeValueRoot, RuntimeValueRoot)>,
    ) -> BuiltinTaskPoll {
        match self.operation {
            PatternListOperation::IsEmpty => rooted_pattern_predicate(context, item.is_none()),
            PatternListOperation::TryUncons => match item {
                Some((head, tail)) => context.with_value_access(|access| {
                    BuiltinTaskPoll::Ready(pattern_success_in(
                        access.values(),
                        Value::Dict(
                            Dict::new_sync()
                                .insert((*keys::HEAD).clone(), access.clone_root(&head))
                                .insert((*keys::TAIL).clone(), access.clone_root(&tail)),
                        ),
                    ))
                }),
                None => rooted_pattern_failure(context),
            },
            PatternListOperation::IsList | PatternListOperation::TryUnsnoc => {
                unreachable!("this pattern-list operation does not use front traversal")
            }
        }
    }

    fn finish_back(
        &self,
        context: &EvaluatorStepContext<'_>,
        item: Option<(RuntimeValueRoot, RuntimeValueRoot)>,
    ) -> BuiltinTaskPoll {
        match item {
            Some((init, last)) => context.with_value_access(|access| {
                BuiltinTaskPoll::Ready(pattern_success_in(
                    access.values(),
                    Value::Dict(
                        Dict::new_sync()
                            .insert((*keys::INIT).clone(), access.clone_root(&init))
                            .insert((*keys::LAST).clone(), access.clone_root(&last)),
                    ),
                ))
            }),
            None => rooted_pattern_failure(context),
        }
    }
}

enum PatternListShape {
    Binary(Bytes),
    List,
    Other,
}

fn rooted_pattern_predicate(context: &EvaluatorStepContext<'_>, passes: bool) -> BuiltinTaskPoll {
    if passes {
        context.with_value_access(|access| {
            BuiltinTaskPoll::Ready(pattern_success_in(
                access.values(),
                Value::Atom(Atom::from_key(&keys::UNIT)),
            ))
        })
    } else {
        rooted_pattern_failure(context)
    }
}

fn pattern_success_in(access: &RuntimeValueAccess<'_>, value: Value) -> RuntimeValueRoot {
    pattern_effect_in(access, &keys::R, vec![value])
}

fn rooted_pattern_failure(context: &EvaluatorStepContext<'_>) -> BuiltinTaskPoll {
    BuiltinTaskPoll::Ready(
        context.with_value_access(|access| pattern_effect_in(access.values(), &keys::FAIL, vec![])),
    )
}

fn pattern_effect_in(
    access: &RuntimeValueAccess<'_>,
    name: &crate::core::Key,
    arguments: Vec<Value>,
) -> RuntimeValueRoot {
    let crate::core::Key::Atom(name) = name else {
        unreachable!("standard effect request names are atom keys")
    };
    access.root_runtime_value(super::application::effect_value(
        access,
        Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::EffectCall,
            arguments: Arc::from([
                Value::Atom(*name),
                Value::List(List::from_values(arguments)),
            ]),
        }),
    ))
}
