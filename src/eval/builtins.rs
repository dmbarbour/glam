//! Saturation and semantic-family dispatch for core builtins.

mod net;

#[cfg(test)]
pub(in crate::eval) use net::{
    construction_journal_lengths_for_test, construction_state_and_ports_for_test,
    decode_outcome_for_test, initial_state_for_test,
};

use super::sequence::append_values;
use super::*;
use crate::evaluation::EvaluationValueAccess;
#[cfg(test)]
pub(crate) use net::assert_construction_port_family_shape;
pub(in crate::eval) use net::interaction_net_from_netlist_in;
pub(in crate::eval) use net::{
    RegionalBuilderBuiltinMachine, RegionalBuilderEffectPoll, RegionalBuilderEffectRunner,
    RegionalNetConstruction, RegionalNetConstructionPoll,
};

#[cfg(test)]
pub(super) fn apply_builtin(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
    argument: Value,
) -> Result<Value, EvaluationHalt> {
    context.evaluate_test_step(|evaluator| {
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
        // A deferred builtin reduces only by its regional machine: one
        // without would be applied again on demand, and defer again, forever.
        if !super::builtin_machine::RegionalBuiltinMachine::supports(builtin) {
            return Err(EvaluationHalt::new(format!(
                "internal error: {builtin:?} defers without a regional machine"
            )));
        }
        Ok(Value::Lazy(LazyValue::from_builtin_in(
            access.values(),
            BuiltinCall {
                builtin,
                arguments: Arc::from(arguments),
            },
        )))
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
        | Builtin::Spark => deferred(arguments),
        Builtin::Map | Builtin::ListConcat | Builtin::TextLines | Builtin::JournalConflicts => {
            deferred(arguments)
        }
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
        | Builtin::ListTail
        | Builtin::ListReverse => deferred(arguments),
        Builtin::PatternIsList
        | Builtin::PatternListTryUncons
        | Builtin::PatternListTryUnsnoc
        | Builtin::PatternListIsEmpty
        | Builtin::PatternPathEqual
        | Builtin::PatternIsDict
        | Builtin::PatternDictIsEmpty
        | Builtin::PatternDictTryTake
        | Builtin::PatternDictTryTakeOptional
        | Builtin::PatternEqual => deferred(arguments),
        Builtin::ListEffect
        | Builtin::ListEffectReturn
        | Builtin::ListEffectSeq
        | Builtin::ListEffectAlt
        | Builtin::ListEffectCut
        | Builtin::ListEffectFix => {
            let deferred_list = |computation: ListEffectComputation| {
                List::from_thunk(
                    LazyValue::list_effect_computation_in(access.values(), computation).into(),
                )
            };
            match builtin {
                Builtin::ListEffect => {
                    let [effect] = exact(arguments, "list effect")?;
                    Ok(Value::List(deferred_list(ListEffectComputation::Run {
                        effect,
                    })))
                }
                Builtin::ListEffectReturn => {
                    let [value] = exact(arguments, "list effect return")?;
                    Ok(Value::List(List::from_values(vec![value])))
                }
                Builtin::ListEffectSeq => {
                    let [operation, continuation] = exact(arguments, "list effect seq")?;
                    let results = deferred_list(ListEffectComputation::Run { effect: operation });
                    Ok(Value::List(deferred_list(
                        ListEffectComputation::Sequence {
                            results,
                            continuation,
                        },
                    )))
                }
                Builtin::ListEffectAlt => {
                    let [left, right] = exact(arguments, "list effect alt")?;
                    let left = deferred_list(ListEffectComputation::Run { effect: left });
                    let right = deferred_list(ListEffectComputation::Run { effect: right });
                    Ok(Value::List(List::concat(left, right)))
                }
                Builtin::ListEffectCut => {
                    let [operation] = exact(arguments, "list effect cut")?;
                    Ok(Value::List(deferred_list(ListEffectComputation::Cut {
                        operation,
                    })))
                }
                Builtin::ListEffectFix => {
                    let [function] = exact(arguments, "list effect fix")?;
                    Ok(Value::List(deferred_list(
                        ListEffectComputation::FixFunction {
                            function,
                            alternative: 0,
                        },
                    )))
                }
                _ => unreachable!("list-effect branch received another builtin"),
            }
        }
        Builtin::Handler
        | Builtin::HandlerReturn
        | Builtin::HandlerSeq
        | Builtin::HandlerAlt
        | Builtin::HandlerFail
        | Builtin::HandlerCut
        | Builtin::HandlerGet
        | Builtin::HandlerSet
        | Builtin::HandlerFix
        | Builtin::HandlerReset
        | Builtin::HandlerShift
        | Builtin::HandlerContinuation
        | Builtin::HandlerResume
        | Builtin::HandlerFixApply
        | Builtin::HandlerFixRun
        | Builtin::HandlerScoped
        | Builtin::HandlerConst
        | Builtin::HandlerClose => {
            super::handler_machine::apply_handler_builtin_in(access, builtin, arguments)
        }
        Builtin::MacroScoped | Builtin::MacroLog => {
            super::macro_machine::apply_macro_builtin_in(access, builtin, arguments)
        }
        Builtin::MacroOperation | Builtin::MacroOperationOn => deferred(arguments),
        Builtin::TokenText
        | Builtin::TokenRegex
        | Builtin::TokenTextSpan
        | Builtin::TokenAny
        | Builtin::TokenEnd => deferred(arguments),
        Builtin::IfResult | Builtin::MatchResult => deferred(arguments),
        Builtin::DictSingleton
        | Builtin::DictUnion
        | Builtin::DictUpdate
        | Builtin::DictPairs
        | Builtin::DictFromEntries
        | Builtin::DictFromPairs
        | Builtin::DictLookup
        | Builtin::DictDefines
        | Builtin::DictDefault
        | Builtin::DictInsert
        | Builtin::DictRemove
        | Builtin::DictUpdateWith
        | Builtin::DictShallowUnion
        | Builtin::MergeDuplicate => deferred(arguments),
        Builtin::ObjectSpec
        | Builtin::ObjectLocalName
        | Builtin::DiagnosticObject
        | Builtin::DiagnosticContextDefs
        | Builtin::ObjectWithDefs
        | Builtin::ObjectComposedDefs
        | Builtin::ObjectOverrideDefs
        | Builtin::ObjectInstance
        | Builtin::ObjectInstanceFromParts
        | Builtin::ObjectDefaultDefs
        | Builtin::ObjectDictDefs
        | Builtin::ObjectFromDict => deferred(arguments),
        Builtin::Fixpoint
        | Builtin::EffectApply
        | Builtin::EffectCall
        | Builtin::EffectMap
        | Builtin::EffectMapRun
        | Builtin::EffectMapContinue => deferred(arguments),
        Builtin::InteractionNet | Builtin::InteractionNetFromNetlist | Builtin::NetArity => {
            deferred(arguments)
        }
        Builtin::InteractionNetBuilderReturn
        | Builtin::InteractionNetBuilderSeq
        | Builtin::InteractionNetBuilderContinue
        | Builtin::InteractionNetBuilderAlt
        | Builtin::InteractionNetBuilderFail
        | Builtin::InteractionNetBuilderCut
        | Builtin::InteractionNetBuilderResume
        | Builtin::InteractionNetBuilderResumeApply
        | Builtin::InteractionNetBuilderFix
        | Builtin::InteractionNetBuilderFixApply
        | Builtin::InteractionNetBuilderFixRun
        | Builtin::InteractionNetBuilderFixRestore => {
            net::apply_builder_builtin_in(access.values(), builtin, arguments)
        }
        Builtin::InteractionNetBuilderGet
        | Builtin::InteractionNetBuilderSet
        | Builtin::InteractionNetBuilderReset
        | Builtin::InteractionNetBuilderShift
        | Builtin::InteractionNetBuilderBind
        | Builtin::InteractionNetBuilderCopy
        | Builtin::InteractionNetBuilderData
        | Builtin::InteractionNetBuilderWire
        | Builtin::InteractionNetBuilderRun => deferred(arguments),
        Builtin::InspectOrigin | Builtin::CompileSource => deferred(arguments),
        Builtin::MixinCompose => {
            let [outer, inner, base, self_value] = exact(arguments, "mixin composition")?;
            let values = access.values();
            let inner = Value::Lazy(LazyValue::from_application_in(
                values,
                inner,
                Arc::from([base, values.duplicate_value(&self_value)]),
            ));
            Ok(Value::Lazy(LazyValue::from_application_in(
                values,
                outer,
                Arc::from([inner, self_value]),
            )))
        }
        Builtin::AssertUnit => deferred(arguments),
        Builtin::Anno => deferred(arguments),
    }
}

fn exact<const N: usize, T>(arguments: Vec<T>, name: &str) -> Result<[T; N], EvaluationHalt> {
    arguments.try_into().map_err(|_| {
        EvaluationHalt::new(format!(
            "{name} builtin received the wrong number of arguments"
        ))
    })
}
