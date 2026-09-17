//! Saturation and semantic-family dispatch for core builtins.

mod effect;
mod list_effect;
mod net;
mod object;

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
        | Builtin::ListEffectFix => list_effect::apply(context, builtin, arguments),
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
        | Builtin::ObjectFromDict
        | Builtin::ObjectLocalName
        | Builtin::ObjectInstanceFromParts
        | Builtin::ObjectInstance
        | Builtin::DiagnosticObject
        | Builtin::ObjectDefaultDefs
        | Builtin::ObjectDictDefs
        | Builtin::ObjectWithDefs
        | Builtin::ObjectComposedDefs
        | Builtin::ObjectOverrideDefs => object::apply(context, builtin, arguments),
        Builtin::Fixpoint | Builtin::EffectApply | Builtin::EffectCall => {
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
        Builtin::EffectMap | Builtin::EffectMapRun | Builtin::EffectMapContinue => {
            effect::apply(context, builtin, arguments)
        }
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
