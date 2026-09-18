//! Saturation and semantic-family dispatch for core builtins.

mod net;

use super::sequence::append_values;
use super::*;
use crate::evaluation::EvaluationValueAccess;
#[cfg(test)]
pub(crate) use net::assert_construction_port_family_shape;
pub(super) use net::{NetConstructionMachine, NetConstructionPoll};

#[cfg(test)]
pub(super) fn apply_builtin(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
    argument: Value,
) -> Result<Value, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| {
        evaluator
            .with_value_access(|access| apply_builtin_in(&access, builtin, arguments, argument))
    })
}

pub(super) fn apply_builtin_in(
    access: &EvaluationValueAccess<'_>,
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

    let deferred = |arguments| {
        Value::Lazy(LazyValue::from_builtin_in(
            access.values(),
            BuiltinCall {
                builtin,
                arguments: Arc::from(arguments),
            },
        ))
    };

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
        | Builtin::Spark => Ok(deferred(arguments)),
        Builtin::Map | Builtin::ListConcat | Builtin::TextLines => Ok(deferred(arguments)),
        Builtin::Append => {
            let [left, right] = exact(arguments, "append")?;
            append_values(access.values(), left, right)
        }
        Builtin::Slice
        | Builtin::ListLen
        | Builtin::ListSplit
        | Builtin::ListSplitEnd
        | Builtin::ListAt
        | Builtin::ListHead
        | Builtin::ListTail => Ok(deferred(arguments)),
        Builtin::PatternIsList
        | Builtin::PatternListTryUncons
        | Builtin::PatternListTryUnsnoc
        | Builtin::PatternListIsEmpty
        | Builtin::PatternPathEqual
        | Builtin::PatternIsDict
        | Builtin::PatternDictIsEmpty
        | Builtin::PatternDictTryTake
        | Builtin::PatternDictTryTakeOptional
        | Builtin::PatternEqual => Ok(deferred(arguments)),
        Builtin::ListEffect
        | Builtin::ListEffectReturn
        | Builtin::ListEffectSeq
        | Builtin::ListEffectAlt
        | Builtin::ListEffectCut
        | Builtin::ListEffectFix => {
            let deferred_list = |label: &'static str, computation: ListEffectComputation| {
                List::from_thunk(
                    LazyValue::list_effect_computation_in(access.values(), label, computation)
                        .into(),
                )
            };
            match builtin {
                Builtin::ListEffect => {
                    let [effect] = exact(arguments, "list effect")?;
                    Ok(Value::List(deferred_list(
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
                    let results = deferred_list(
                        "list effect",
                        ListEffectComputation::Run { effect: operation },
                    );
                    Ok(Value::List(deferred_list(
                        "list effect seq",
                        ListEffectComputation::Sequence {
                            results,
                            continuation,
                        },
                    )))
                }
                Builtin::ListEffectAlt => {
                    let [left, right] = exact(arguments, "list effect alt")?;
                    let left =
                        deferred_list("list effect", ListEffectComputation::Run { effect: left });
                    let right =
                        deferred_list("list effect", ListEffectComputation::Run { effect: right });
                    Ok(Value::List(List::concat(left, right)))
                }
                Builtin::ListEffectCut => {
                    let [operation] = exact(arguments, "list effect cut")?;
                    Ok(Value::List(deferred_list(
                        "list effect cut",
                        ListEffectComputation::Cut { operation },
                    )))
                }
                Builtin::ListEffectFix => {
                    let [function] = exact(arguments, "list effect fix")?;
                    Ok(Value::List(deferred_list(
                        "list effect fix",
                        ListEffectComputation::FixFunction { function },
                    )))
                }
                _ => unreachable!("list-effect branch received another builtin"),
            }
        }
        Builtin::IfResult | Builtin::MatchResult => Ok(deferred(arguments)),
        Builtin::DictSingleton
        | Builtin::DictUnion
        | Builtin::DictUpdate
        | Builtin::MergeDuplicate => Ok(deferred(arguments)),
        Builtin::ObjectSpec
        | Builtin::ObjectLocalName
        | Builtin::DiagnosticObject
        | Builtin::ObjectWithDefs
        | Builtin::ObjectComposedDefs
        | Builtin::ObjectOverrideDefs
        | Builtin::ObjectInstance
        | Builtin::ObjectInstanceFromParts
        | Builtin::ObjectDefaultDefs
        | Builtin::ObjectDictDefs
        | Builtin::ObjectFromDict => Ok(deferred(arguments)),
        Builtin::Fixpoint
        | Builtin::EffectApply
        | Builtin::EffectCall
        | Builtin::EffectMap
        | Builtin::EffectMapRun
        | Builtin::EffectMapContinue => Ok(deferred(arguments)),
        Builtin::InteractionNet | Builtin::NetArity => Ok(deferred(arguments)),
        Builtin::InspectOrigin => Ok(deferred(arguments)),
        Builtin::AssertUnit => Ok(deferred(arguments)),
        Builtin::Anno => Ok(deferred(arguments)),
    }
}

fn exact<const N: usize, T>(arguments: Vec<T>, name: &str) -> Result<[T; N], EvaluationHalt> {
    arguments.try_into().map_err(|_| {
        EvaluationHalt::new(format!(
            "{name} builtin received the wrong number of arguments"
        ))
    })
}
