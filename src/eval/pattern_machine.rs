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
    access.root_runtime_value(super::application::effect_value(Value::PartialBuiltin(
        BuiltinCall {
            builtin: Builtin::EffectCall,
            arguments: Arc::from([
                Value::Atom(*name),
                Value::List(List::from_values(arguments)),
            ]),
        },
    )))
}
