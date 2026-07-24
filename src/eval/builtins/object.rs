use super::super::*;
use super::dict::eval_dict_union_builtin;

mod implementation;

pub(in crate::eval) use implementation::construct_object_instance as construct_fixpoint_object;
use implementation::*;

pub(super) fn apply(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    match builtin {
        Builtin::ObjectSpec => {
            let [value] = super::exact(arguments, "object spec")?;
            eval_object_spec_builtin(context, &value)
        }
        Builtin::ObjectFromDict => {
            let [value] = super::exact(arguments, "object_from_dict")?;
            eval_object_from_dict_builtin(context, &value)
        }
        Builtin::ObjectLocalName => {
            let [host, parts] = super::exact(arguments, "object local name")?;
            eval_object_local_name_builtin(context, &host, &parts)
        }
        Builtin::ObjectAbstractFromParts => {
            let [name, deps, defs] = super::exact(arguments, "abstract object from parts")?;
            Ok(eval_object_abstract_from_parts_builtin(name, deps, defs))
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
            eval_value(context, &base)
        }
        Builtin::ObjectDictDefs => {
            let [dict, base, _self_value] =
                super::exact(arguments, "dictionary object definitions")?;
            eval_dict_union_builtin(context, &base, &dict)
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
