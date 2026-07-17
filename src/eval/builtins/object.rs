use super::super::*;
use super::dict::eval_dict_union_builtin;

mod implementation;

use implementation::*;

pub(super) fn apply(builtin: Builtin, arguments: Vec<Value>) -> Result<Value, EvalError> {
    match builtin {
        Builtin::ObjectSpec => {
            let [value] = super::exact(arguments, "object spec")?;
            eval_object_spec_builtin(&value)
        }
        Builtin::ObjectLocalName => {
            let [host, parts] = super::exact(arguments, "object local name")?;
            eval_object_local_name_builtin(&host, &parts)
        }
        Builtin::ObjectInstanceFromParts => {
            let [name, deps, defs] = super::exact(arguments, "object instance from parts")?;
            eval_object_instance_from_parts_builtin(name, deps, defs)
        }
        Builtin::ObjectInstance => {
            let [spec] = super::exact(arguments, "object instance")?;
            eval_object_instance_builtin(&spec)
        }
        Builtin::ObjectDefaultDefs => {
            let [base, _self_value] = super::exact(arguments, "default object definitions")?;
            eval_value(&base)
        }
        Builtin::ObjectDictDefs => {
            let [dict, base, _self_value] =
                super::exact(arguments, "dictionary object definitions")?;
            eval_dict_union_builtin(&base, &dict)
        }
        Builtin::ObjectWithDefs => {
            let [object, extension_defs] = super::exact(arguments, "object with definitions")?;
            eval_object_with_defs_builtin(&object, extension_defs)
        }
        Builtin::ObjectComposedDefs => {
            let [prior_defs, extension_defs, base, self_value] =
                super::exact(arguments, "composed object definitions")?;
            eval_object_composed_defs_builtin(prior_defs, extension_defs, base, self_value)
        }
        Builtin::ObjectOverrideDefs => {
            let [updates, base, _self_value] =
                super::exact(arguments, "overriding object definitions")?;
            eval_object_override_defs_builtin(&updates, &base)
        }
        _ => unreachable!("object dispatcher received another builtin"),
    }
}
