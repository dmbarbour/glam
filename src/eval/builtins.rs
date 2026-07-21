//! Saturation and semantic-family dispatch for core builtins.

mod annotation;
mod comparison;
mod dict;
mod effect;
mod list;
mod list_effect;
mod net;
mod numeric;
mod object;
mod strategy;

use super::*;
pub(super) use annotation::is_undefined_value;
pub(super) use object::construct_fixpoint_object;

pub(super) fn apply_builtin(
    context: &EvalContext,
    builtin: Builtin,
    mut arguments: Vec<Value>,
    argument: Value,
) -> Result<Value, EvalError> {
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
        | Builtin::Mod => numeric::apply(context, builtin, arguments),
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
        | Builtin::ListHead
        | Builtin::ListTail
        | Builtin::TextLines => list::apply(context, builtin, arguments),
        Builtin::ListEffect
        | Builtin::ListEffectReturn
        | Builtin::ListEffectSeq
        | Builtin::ListEffectAlt
        | Builtin::ListEffectCut
        | Builtin::ListEffectFix => list_effect::apply(context, builtin, arguments),
        Builtin::DictSingleton
        | Builtin::DictUnion
        | Builtin::DictUpdate
        | Builtin::MergeDuplicate => dict::apply(context, builtin, arguments),
        Builtin::ObjectSpec
        | Builtin::ObjectLocalName
        | Builtin::ObjectInstanceFromParts
        | Builtin::ObjectInstance
        | Builtin::ObjectDefaultDefs
        | Builtin::ObjectDictDefs
        | Builtin::ObjectWithDefs
        | Builtin::ObjectComposedDefs
        | Builtin::ObjectOverrideDefs => object::apply(context, builtin, arguments),
        Builtin::Fixpoint
        | Builtin::EffectApply
        | Builtin::EffectCall
        | Builtin::EffectMap
        | Builtin::EffectMapRun
        | Builtin::EffectMapContinue => effect::apply(context, builtin, arguments),
        Builtin::Seq | Builtin::Spark => strategy::apply(context, builtin, arguments),
        Builtin::NetArity => net::apply(context, arguments),
        Builtin::Anno => annotation::apply(context, arguments),
    }
}

fn exact<const N: usize>(arguments: Vec<Value>, name: &str) -> Result<[Value; N], EvalError> {
    arguments.try_into().map_err(|_| {
        EvalError::new(format!(
            "{name} builtin received the wrong number of arguments"
        ))
    })
}
