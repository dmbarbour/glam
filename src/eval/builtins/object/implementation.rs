use super::super::super::*;

pub(super) fn eval_object_instance_builtin(
    spec: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let spec_dict = object_spec_dict(spec)?;
    let specs = object_application_order(&spec_dict, local_env)?;

    let handle = LazyValue::pending("object self");
    let self_marker = Value::Lazy(handle.clone());
    let mut base = Value::Dict(crate::core::Dict::new_sync());
    for spec in specs {
        let defs = spec
            .get(&*keys::DEFS)
            .cloned()
            .unwrap_or_else(default_object_defs_value);
        let mixed = apply_value(eval_value(&defs)?, base, local_env)?;
        let mixed = apply_value(eval_value(&mixed)?, self_marker.clone(), local_env)?;
        let Value::Dict(mixed_dict) = force_value_shell(&mixed)? else {
            return Err(EvalError::new(
                "object definition mixin must produce a dictionary",
            ));
        };
        base = Value::Dict(mixed_dict);
    }

    let Value::Dict(base_dict) = base else {
        return Err(EvalError::new("object base is not a dictionary"));
    };
    let object = Value::Dict(base_dict.insert((*keys::SPEC).clone(), Value::Dict(spec_dict)));
    handle
        .set(object.clone())
        .map_err(|_| EvalError::new("object instance initialized twice"))?;
    Ok(object)
}

pub(super) fn eval_object_instance_from_parts_builtin(
    name: Value,
    deps: Value,
    defs: Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let spec = crate::core::Dict::new_sync()
        .insert((*keys::NAME).clone(), name)
        .insert((*keys::DEPS).clone(), deps)
        .insert((*keys::DEFS).clone(), defs);
    eval_object_instance_builtin(&Value::Dict(spec), local_env)
}

/// Re-instantiates an object with an additional stateless definitions mixin.
///
/// The composed definitions are retained in the resulting `spec`; directly
/// updating the instance dictionary would lose the extension when a later
/// observer inherits the object again.
pub(super) fn eval_object_with_defs_builtin(
    object: &Value,
    extension_defs: Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let spec = object_spec_dict(&eval_object_spec_builtin(object)?)?;
    let name = spec
        .get(&*keys::NAME)
        .cloned()
        .ok_or_else(|| EvalError::new("object specification requires a name"))?;
    let deps = spec
        .get(&*keys::DEPS)
        .cloned()
        .unwrap_or_else(|| Value::List(List::empty()));
    let prior_defs = spec
        .get(&*keys::DEFS)
        .cloned()
        .unwrap_or_else(default_object_defs_value);
    let composed_defs = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::ObjectComposedDefs,
        arguments: Arc::from([prior_defs, extension_defs]),
    });
    eval_object_instance_from_parts_builtin(name, deps, composed_defs, local_env)
}

pub(super) fn eval_object_composed_defs_builtin(
    prior_defs: Value,
    extension_defs: Value,
    base: Value,
    self_value: Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let prior = apply_value(prior_defs, base, local_env)?;
    let prior = apply_value(prior, self_value.clone(), local_env)?;
    let extended = apply_value(extension_defs, prior, local_env)?;
    apply_value(extended, self_value, local_env)
}

/// Implements the small right-biased record mixin used for assembler-owned
/// diagnostic fields. It is an internal definitions adapter, not the language
/// `with` surface or its assertion policy.
pub(super) fn eval_object_override_defs_builtin(
    updates: &Value,
    base: &Value,
) -> Result<Value, EvalError> {
    let updates = force_value_shell(updates)?;
    let base = force_value_shell(base)?;
    let (Value::Dict(updates), Value::Dict(base)) = (updates, base) else {
        return Err(EvalError::new(
            "object override definitions require dictionary values",
        ));
    };
    Ok(Value::Dict(override_dict(&base, &updates)))
}

fn override_dict(base: &crate::core::Dict, updates: &crate::core::Dict) -> crate::core::Dict {
    updates.iter().fold(base.clone(), |base, (key, update)| {
        let update = match (base.get(key), update) {
            (Some(Value::Dict(prior)), Value::Dict(update)) => {
                Value::Dict(override_dict(prior, update))
            }
            _ => update.clone(),
        };
        base.insert(key.clone(), update)
    })
}

pub(super) fn eval_object_spec_builtin(value: &Value) -> Result<Value, EvalError> {
    let value = force_value_shell(value)?;
    let Value::Dict(dict) = value else {
        return Err(EvalError::new(
            "object spec builtin requires an object or dictionary value",
        ));
    };

    if let Some(spec) = dict.get(&*keys::SPEC) {
        let spec = force_value_shell(spec)?;
        if !is_undefined_dict_value(&spec) {
            return Ok(spec);
        }
    }

    Ok(dict_object_spec(dict))
}

pub(super) fn eval_object_local_name_builtin(
    host: &Value,
    parts: &Value,
) -> Result<Value, EvalError> {
    let host_spec = eval_object_spec_builtin(host)?;
    let host_spec = object_spec_dict(&host_spec)?;
    let Some(host_name) = host_spec.get(&*keys::NAME).cloned() else {
        return Err(EvalError::new("object specification requires a name"));
    };

    let mut name_parts = vec![eval_value(&host_name)?];
    name_parts.extend(match force_value_shell(parts)? {
        Value::List(parts) => list_to_value_items(&parts)?,
        Value::Dict(dict) if dict.is_empty() => Vec::new(),
        _ => {
            return Err(EvalError::new(
                "object local name builtin requires a list of name parts",
            ));
        }
    });
    Ok(Value::List(List::from_values(name_parts)))
}

fn object_spec_dict(spec: &Value) -> Result<crate::core::Dict, EvalError> {
    let spec = force_value_shell(spec)?;
    let Value::Dict(spec_dict) = spec else {
        return Err(EvalError::new(
            "object instance builtin requires a specification dictionary",
        ));
    };
    Ok(spec_dict)
}

fn dict_object_spec(dict: crate::core::Dict) -> Value {
    let defs = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::ObjectDictDefs,
        arguments: Arc::from([Value::Dict(dict)]),
    });
    let spec = crate::core::Dict::new_sync()
        .insert(
            (*keys::NAME).clone(),
            Value::Dict(crate::core::Dict::new_sync()),
        )
        .insert((*keys::DEPS).clone(), Value::List(List::empty()))
        .insert((*keys::DEFS).clone(), defs);
    Value::Dict(spec)
}

fn object_application_order(
    spec: &crate::core::Dict,
    local_env: &[Value],
) -> Result<Vec<crate::core::Dict>, EvalError> {
    let mut seen = BTreeMap::new();
    let mut next_anonymous_id = 0;
    let mut linearized =
        object_c3_linearization(spec, local_env, &mut seen, &mut next_anonymous_id)?;
    linearized.reverse();
    Ok(linearized
        .into_iter()
        .map(|entry| entry.spec)
        .collect::<Vec<_>>())
}

#[derive(Clone)]
struct LinearizedObjectSpec {
    spec: crate::core::Dict,
    name: Key,
    anonymous_id: Option<u64>,
}

impl LinearizedObjectSpec {
    fn new(
        spec: crate::core::Dict,
        local_env: &[Value],
        next_anonymous_id: &mut u64,
    ) -> Result<Self, EvalError> {
        let name = object_spec_name(&spec, local_env)?;
        let anonymous_id = if is_anonymous_object_name(&name) {
            let id = *next_anonymous_id;
            *next_anonymous_id += 1;
            Some(id)
        } else {
            None
        };
        Ok(Self {
            spec,
            name,
            anonymous_id,
        })
    }
}

fn object_c3_linearization(
    spec: &crate::core::Dict,
    local_env: &[Value],
    seen: &mut BTreeMap<Key, ()>,
    next_anonymous_id: &mut u64,
) -> Result<Vec<LinearizedObjectSpec>, EvalError> {
    let entry = LinearizedObjectSpec::new(spec.clone(), local_env, next_anonymous_id)?;
    if entry.anonymous_id.is_none() {
        remember_object_spec(&entry.name, spec, seen)?;
    }
    let deps = spec
        .get(&*keys::DEPS)
        .cloned()
        .unwrap_or_else(|| Value::List(List::empty()));
    let deps = object_dep_specs(&deps)?;
    let mut sequences: Vec<Vec<LinearizedObjectSpec>> = Vec::new();
    let mut direct_deps = Vec::new();
    let mut saw_named_dep = false;
    for dep_spec in &deps {
        let dep_spec = object_spec_dict(dep_spec)?;
        let dep_linearization =
            object_c3_linearization(&dep_spec, local_env, seen, next_anonymous_id)?;
        let dep_entry = dep_linearization
            .first()
            .cloned()
            .ok_or_else(|| EvalError::new("object dependency linearization was empty"))?;
        if dep_entry.anonymous_id.is_some() {
            if saw_named_dep {
                return Err(EvalError::new(
                    "anonymous object dependencies must appear before named object dependencies",
                ));
            }
        } else {
            saw_named_dep = true;
        }
        direct_deps.push(dep_entry);
        sequences.push(dep_linearization);
    }
    sequences.push(direct_deps);

    let mut linearized = vec![entry];
    linearized.extend(c3_merge(sequences, local_env)?);
    Ok(linearized)
}

fn c3_merge(
    mut sequences: Vec<Vec<LinearizedObjectSpec>>,
    _local_env: &[Value],
) -> Result<Vec<LinearizedObjectSpec>, EvalError> {
    let mut result = Vec::new();

    loop {
        sequences.retain(|sequence| !sequence.is_empty());
        if sequences.is_empty() {
            return Ok(result);
        }

        let mut selected = None;
        'candidate: for sequence in &sequences {
            let candidate = &sequence[0];
            for other in &sequences {
                if other
                    .iter()
                    .skip(1)
                    .any(|spec| same_linearized_object_spec(spec, candidate))
                {
                    continue 'candidate;
                }
            }
            selected = Some(candidate.clone());
            break;
        }

        let Some(selected_spec) = selected else {
            return Err(EvalError::new(
                "object dependencies have inconsistent C3 linearization",
            ));
        };
        result.push(selected_spec.clone());

        for sequence in &mut sequences {
            if sequence
                .first()
                .is_some_and(|spec| same_linearized_object_spec(spec, &selected_spec))
            {
                sequence.remove(0);
            }
        }
    }
}

fn same_linearized_object_spec(left: &LinearizedObjectSpec, right: &LinearizedObjectSpec) -> bool {
    match (left.anonymous_id, right.anonymous_id) {
        (Some(left), Some(right)) => left == right,
        (None, None) => left.name == right.name,
        _ => false,
    }
}

fn object_spec_name(spec: &crate::core::Dict, local_env: &[Value]) -> Result<Key, EvalError> {
    let Some(name) = spec.get(&*keys::NAME) else {
        return Err(EvalError::new("object specification requires a name"));
    };
    let name = force_value_shell(name)?;
    value_to_key(&name, local_env)
}

fn is_anonymous_object_name(name: &Key) -> bool {
    matches!(name, Key::Dict(entries) if entries.is_empty())
}

fn remember_object_spec(
    name: &Key,
    _spec: &crate::core::Dict,
    seen: &mut BTreeMap<Key, ()>,
) -> Result<(), EvalError> {
    seen.insert(name.clone(), ());
    Ok(())
}

fn object_dep_specs(deps: &Value) -> Result<Vec<Value>, EvalError> {
    match force_value_shell(deps)? {
        Value::List(list) => list_to_value_items(&list),
        Value::Dict(dict) if dict.is_empty() => Ok(Vec::new()),
        _ => Err(EvalError::new(
            "object specification deps must evaluate to a list",
        )),
    }
}

fn default_object_defs_value() -> Value {
    Value::Builtin(Builtin::ObjectDefaultDefs)
}
