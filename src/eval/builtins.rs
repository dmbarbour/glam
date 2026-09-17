//! Saturation and semantic-family dispatch for core builtins.

mod net;
mod object;

use super::sequence::append_values;
use super::*;
pub(super) use net::NetConstructionMachine;
#[cfg(test)]
pub(crate) use net::assert_construction_port_family_shape;

#[cfg(test)]
pub(super) fn apply_builtin(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
    argument: Value,
) -> Result<Value, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| {
        apply_builtin_in(evaluator, builtin, arguments, argument)
    })
}

pub(super) fn apply_builtin_in(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    mut arguments: Vec<Value>,
    argument: Value,
) -> Result<Value, EvaluationHalt> {
    arguments.push(argument);
    if arguments.len() < builtin.arity() {
        return Ok(Value::PartialBuiltin(BuiltinCall {
            builtin,
            arguments: Arc::from(arguments),
        }));
    }

    match builtin {
        Builtin::Add
        | Builtin::Subtract
        | Builtin::Multiply
        | Builtin::Divide
        | Builtin::Floor
        | Builtin::Mod
        | Builtin::Greater
        | Builtin::GreaterEqual
        | Builtin::Equal
        | Builtin::NotEqual
        | Builtin::LessEqual
        | Builtin::Less
        | Builtin::Seq
        | Builtin::Spark => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::Map | Builtin::ListConcat | Builtin::TextLines => {
            Ok(Value::Lazy(context.construct_lazy(move |access| {
                LazyValue::from_builtin_in(
                    access,
                    BuiltinCall {
                        builtin,
                        arguments: Arc::from(arguments),
                    },
                )
            })))
        }
        Builtin::Append => {
            let [left, right] = exact(arguments, "append")?;
            context.with_value_access(|access| append_values(access.values(), left, right))
        }
        Builtin::Slice
        | Builtin::ListLen
        | Builtin::ListSplit
        | Builtin::ListSplitEnd
        | Builtin::ListAt
        | Builtin::ListHead
        | Builtin::ListTail => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::PatternIsList
        | Builtin::PatternListTryUncons
        | Builtin::PatternListTryUnsnoc
        | Builtin::PatternListIsEmpty
        | Builtin::PatternPathEqual
        | Builtin::PatternIsDict
        | Builtin::PatternDictIsEmpty
        | Builtin::PatternDictTryTake
        | Builtin::PatternDictTryTakeOptional
        | Builtin::PatternEqual => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::ListEffect
        | Builtin::ListEffectReturn
        | Builtin::ListEffectSeq
        | Builtin::ListEffectAlt
        | Builtin::ListEffectCut
        | Builtin::ListEffectFix => {
            let deferred = |label: &'static str, computation: ListEffectComputation| {
                List::from_thunk(
                    context
                        .construct_lazy(move |access| {
                            LazyValue::list_effect_computation_in(access, label, computation)
                        })
                        .into(),
                )
            };
            match builtin {
                Builtin::ListEffect => {
                    let [effect] = exact(arguments, "list effect")?;
                    Ok(Value::List(deferred(
                        "list effect",
                        ListEffectComputation::Run { effect },
                    )))
                }
                Builtin::ListEffectReturn => {
                    let [value] = exact(arguments, "list effect return")?;
                    Ok(Value::List(List::from_values(vec![value])))
                }
                Builtin::ListEffectSeq => {
                    let [operation, continuation] = exact(arguments, "list effect seq")?;
                    let results = deferred(
                        "list effect",
                        ListEffectComputation::Run { effect: operation },
                    );
                    Ok(Value::List(deferred(
                        "list effect seq",
                        ListEffectComputation::Sequence {
                            results,
                            continuation,
                        },
                    )))
                }
                Builtin::ListEffectAlt => {
                    let [left, right] = exact(arguments, "list effect alt")?;
                    let left = deferred("list effect", ListEffectComputation::Run { effect: left });
                    let right =
                        deferred("list effect", ListEffectComputation::Run { effect: right });
                    Ok(Value::List(List::concat(left, right)))
                }
                Builtin::ListEffectCut => {
                    let [operation] = exact(arguments, "list effect cut")?;
                    Ok(Value::List(deferred(
                        "list effect cut",
                        ListEffectComputation::Cut { operation },
                    )))
                }
                Builtin::ListEffectFix => {
                    let [function] = exact(arguments, "list effect fix")?;
                    Ok(Value::List(deferred(
                        "list effect fix",
                        ListEffectComputation::FixFunction { function },
                    )))
                }
                _ => unreachable!("list-effect branch received another builtin"),
            }
        }
        Builtin::IfResult | Builtin::MatchResult => {
            Ok(Value::Lazy(context.construct_lazy(move |access| {
                LazyValue::from_builtin_in(
                    access,
                    BuiltinCall {
                        builtin,
                        arguments: Arc::from(arguments),
                    },
                )
            })))
        }
        Builtin::DictSingleton
        | Builtin::DictUnion
        | Builtin::DictUpdate
        | Builtin::MergeDuplicate => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::ObjectSpec
        | Builtin::ObjectLocalName
        | Builtin::DiagnosticObject
        | Builtin::ObjectWithDefs
        | Builtin::ObjectComposedDefs => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::ObjectFromDict
        | Builtin::ObjectInstanceFromParts
        | Builtin::ObjectInstance
        | Builtin::ObjectDefaultDefs
        | Builtin::ObjectDictDefs
        | Builtin::ObjectOverrideDefs => object::apply(context, builtin, arguments),
        Builtin::Fixpoint
        | Builtin::EffectApply
        | Builtin::EffectCall
        | Builtin::EffectMap
        | Builtin::EffectMapRun
        | Builtin::EffectMapContinue => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::InteractionNet | Builtin::NetArity => net::apply(context, builtin, arguments),
        Builtin::InspectOrigin => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::AssertUnit => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::Anno => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
    }
}

fn exact<const N: usize, T>(arguments: Vec<T>, name: &str) -> Result<[T; N], EvaluationHalt> {
    arguments.try_into().map_err(|_| {
        EvaluationHalt::new(format!(
            "{name} builtin received the wrong number of arguments"
        ))
    })
}
