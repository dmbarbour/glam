//! Saturation and semantic-family dispatch for core builtins.

mod annotation;
mod comparison;
mod dict;
mod effect;
mod list;
mod list_effect;
mod numeric;
mod object;

use super::*;
pub(super) use annotation::is_undefined_value;

pub(super) fn apply_builtin(
    builtin: Builtin,
    mut arguments: Vec<Value>,
    argument: Value,
    local_env: &[Value],
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
        | Builtin::Mod => numeric::apply(builtin, arguments, local_env),
        Builtin::Greater
        | Builtin::GreaterEqual
        | Builtin::Equal
        | Builtin::NotEqual
        | Builtin::LessEqual
        | Builtin::Less => comparison::apply(builtin, arguments, local_env),
        Builtin::Append
        | Builtin::Slice
        | Builtin::Map
        | Builtin::ListLen
        | Builtin::ListSplit
        | Builtin::ListSplitEnd
        | Builtin::ListHead
        | Builtin::ListTail => list::apply(builtin, arguments, local_env),
        Builtin::ListEffect
        | Builtin::ListEffectReturn
        | Builtin::ListEffectSeq
        | Builtin::ListEffectAlt
        | Builtin::ListEffectCut
        | Builtin::ListEffectFix => list_effect::apply(builtin, arguments, local_env),
        Builtin::DictSingleton
        | Builtin::DictUnion
        | Builtin::DictUpdate
        | Builtin::MergeDuplicate => dict::apply(builtin, arguments, local_env),
        Builtin::ObjectSpec
        | Builtin::ObjectLocalName
        | Builtin::ObjectInstanceFromParts
        | Builtin::ObjectInstance
        | Builtin::ObjectDefaultDefs
        | Builtin::ObjectDictDefs
        | Builtin::ObjectWithDefs
        | Builtin::ObjectComposedDefs
        | Builtin::ObjectOverrideDefs => object::apply(builtin, arguments, local_env),
        Builtin::Fixpoint | Builtin::EffectApply | Builtin::EffectCall => {
            effect::apply(builtin, arguments, local_env)
        }
        Builtin::Anno => annotation::apply(arguments, local_env),
    }
}

fn exact<const N: usize>(arguments: Vec<Value>, name: &str) -> Result<[Value; N], EvalError> {
    arguments.try_into().map_err(|_| {
        EvalError::new(format!(
            "{name} builtin received the wrong number of arguments"
        ))
    })
}
