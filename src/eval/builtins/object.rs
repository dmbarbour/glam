use super::super::*;
use crate::eval::dict_machine::merge_dicts_in;

mod implementation;

use implementation::*;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::ObjectFromDict => {
            let [value] = super::exact(arguments, "object_from_dict")?;
            eval_object_from_dict_builtin(context, &value)
        }
        Builtin::ObjectInstanceFromParts => {
            let [name, deps, defs] = super::exact(arguments, "object instance from parts")?;
            eval_object_instance_from_parts_builtin(context, name, deps, defs)
        }
        Builtin::ObjectInstance => {
            let [spec] = super::exact(arguments, "object instance")?;
            eval_object_instance_builtin(context, &spec)
        }
        Builtin::ObjectDefaultDefs => {
            let [base, _self_value] = super::exact(arguments, "default object definitions")?;
            eval_value_in(context, &base)
        }
        Builtin::ObjectDictDefs => {
            let [dict, base, _self_value] =
                super::exact(arguments, "dictionary object definitions")?;
            let base = eval_value_in(context, &base)?;
            let dict = eval_value_in(context, &dict)?;
            let (Value::Dict(base), Value::Dict(dict)) = (base, dict) else {
                return Err(EvaluationHalt::new(
                    "dictionary union requires dictionary values",
                ));
            };
            Ok(context.with_value_access(|access| {
                Value::Dict(merge_dicts_in(access.values(), &base, &dict))
            }))
        }
        Builtin::ObjectWithDefs => {
            let [object, extension_defs] = super::exact(arguments, "object with definitions")?;
            eval_object_with_defs_builtin(context, &object, extension_defs)
        }
        Builtin::ObjectComposedDefs => {
            let [prior_defs, extension_defs, base, self_value] =
                super::exact(arguments, "composed object definitions")?;
            eval_object_composed_defs_builtin(context, prior_defs, extension_defs, base, self_value)
        }
        Builtin::ObjectOverrideDefs => {
            let [updates, base, _self_value] =
                super::exact(arguments, "overriding object definitions")?;
            eval_object_override_defs_builtin(context, &updates, &base)
        }
        _ => unreachable!("object dispatcher received another builtin"),
    }
}
