//! Saturation and semantic-family dispatch for core builtins.

mod annotation;
mod assertion;
mod comparison;
mod dict;
mod effect;
mod list;
mod list_effect;
mod net;
mod object;
mod pattern;
mod strategy;

use super::*;
pub(super) use annotation::is_undefined_value;
pub(super) use net::NetConstructionMachine;
#[cfg(test)]
pub(crate) use net::assert_construction_port_family_shape;
#[cfg(test)]
pub(crate) use strategy::demand as demand_strategy_value;
pub(crate) use strategy::demand_in as demand_strategy_value_in;

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
        | Builtin::Mod => Ok(Value::Lazy(context.construct_lazy(move |access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        }))),
        Builtin::Greater
        | Builtin::GreaterEqual
        | Builtin::Equal
        | Builtin::NotEqual
        | Builtin::LessEqual
        | Builtin::Less => comparison::apply(context, builtin, arguments),
        Builtin::Append
        | Builtin::Slice
        | Builtin::Map
        | Builtin::ListConcat
        | Builtin::ListLen
        | Builtin::ListSplit
        | Builtin::ListSplitEnd
        | Builtin::ListAt
        | Builtin::ListHead
        | Builtin::ListTail
        | Builtin::TextLines => list::apply(context, builtin, arguments),
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
        | Builtin::MergeDuplicate => dict::apply(context, builtin, arguments),
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
        Builtin::Seq | Builtin::Spark => strategy::apply(context.context(), builtin, arguments),
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
