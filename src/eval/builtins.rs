//! Saturation and semantic-family dispatch for core builtins.

mod annotation;
mod assertion;
mod effect;
mod list;
mod list_effect;
mod net;
mod object;
mod pattern;

use super::*;
pub(super) use annotation::is_undefined_value;
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
        Builtin::Map | Builtin::ListConcat => {
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
        Builtin::Append | Builtin::TextLines => list::apply(context, builtin, arguments),
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
        | Builtin::PatternEqual
        | Builtin::PatternPathEqual
        | Builtin::PatternIsDict
        | Builtin::PatternDictTryTake
        | Builtin::PatternDictTryTakeOptional
        | Builtin::PatternDictIsEmpty => pattern::apply(context, builtin, arguments),
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
        Builtin::Fixpoint => effect::apply_fixpoint(context, arguments),
        Builtin::EffectApply
        | Builtin::EffectCall
        | Builtin::EffectMap
        | Builtin::EffectMapRun
        | Builtin::EffectMapContinue => effect::apply(context, builtin, arguments),
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
        Builtin::Anno => annotation::apply(context, arguments),
    }
}

fn exact<const N: usize, T>(arguments: Vec<T>, name: &str) -> Result<[T; N], EvaluationHalt> {
    arguments.try_into().map_err(|_| {
        EvaluationHalt::new(format!(
            "{name} builtin received the wrong number of arguments"
        ))
    })
}
