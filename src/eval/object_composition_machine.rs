//! Object-composition construction.
//!
//! Ordinary extension, composed-definition calls, and recursive override are
//! raw regional state beneath the owning lazy's managed builtin checkpoint.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluationFailure, FixpointComputation, Key, LazyId, LazyValue,
    List, Value, keys, trace_compatibility_value_managed_edges,
};
use crate::evaluation::EvaluationValueAccess;

use super::builtin_machine::RegionalBuiltinPoll;
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalObjectCompositionMachine {
    phase: RegionalCompositionPhase,
    source_owner: LazyId,
}

enum RegionalCompositionPhase {
    WithObject {
        object: RegionalWhnfWork,
        extension_defs: Value,
    },
    WithSpec {
        object: Value,
        spec: RegionalWhnfWork,
        extension_defs: Value,
    },
    ComposedPriorBase {
        application: RegionalWhnfWork,
        extension_defs: Value,
        self_value: Value,
    },
    ComposedExtensionPrior {
        application: RegionalWhnfWork,
        self_value: Value,
    },
    OverrideUpdates {
        updates: RegionalWhnfWork,
        base: Value,
    },
    OverrideBase {
        updates: Value,
        base: RegionalWhnfWork,
    },
    Override(RegionalObjectOverrideMachine),
}

struct RegionalObjectOverrideMachine {
    stack: Vec<RegionalOverrideFrame>,
    source_owner: LazyId,
}

struct RegionalOverrideFrame {
    result: Value,
    updates: Value,
    keys: Vec<Key>,
    next: usize,
    pending: Option<RegionalPendingOverride>,
    return_key: Option<Key>,
}

struct RegionalPendingOverride {
    key: Key,
    update: Value,
    prior: RegionalWhnfWork,
}

impl RegionalObjectCompositionMachine {
    pub(in crate::eval) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::ObjectWithDefs | Builtin::ObjectComposedDefs | Builtin::ObjectOverrideDefs
        )
    }

    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let duplicate = |value: &Value| access.values().duplicate_value(value);
        let phase = match builtin {
            Builtin::ObjectWithDefs => {
                let [object, extension_defs] = arguments else {
                    unreachable!("object extension retains two operands")
                };
                RegionalCompositionPhase::WithObject {
                    object: RegionalWhnfWork::from_focus(access, duplicate(object))
                        .with_source_owner(source_owner),
                    extension_defs: duplicate(extension_defs),
                }
            }
            Builtin::ObjectComposedDefs => {
                let [prior_defs, extension_defs, base, self_value] = arguments else {
                    unreachable!("composed object definitions retain four operands")
                };
                RegionalCompositionPhase::ComposedPriorBase {
                    application: regional_application_in(
                        access,
                        duplicate(prior_defs),
                        &[duplicate(base)],
                        source_owner,
                    ),
                    extension_defs: duplicate(extension_defs),
                    self_value: duplicate(self_value),
                }
            }
            Builtin::ObjectOverrideDefs => {
                let [updates, base, _self_value] = arguments else {
                    unreachable!("object override definitions retain three operands")
                };
                RegionalCompositionPhase::OverrideUpdates {
                    updates: RegionalWhnfWork::from_focus(access, duplicate(updates))
                        .with_source_owner(source_owner),
                    base: duplicate(base),
                }
            }
            _ => unreachable!("regional object composition received another builtin"),
        };
        Self {
            phase,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match &mut self.phase {
            RegionalCompositionPhase::WithObject {
                object,
                extension_defs,
            } => {
                let object = match poll_regional_whnf_in(access, object, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let Value::Dict(object_dict) = &object else {
                    return regional_failure(
                        "ordinary `with` requires a dictionary or object value",
                    );
                };
                let Some(spec) = object_dict
                    .get(&*keys::SPEC)
                    .map(|spec| access.values().duplicate_value(spec))
                else {
                    return RegionalBuiltinPoll::Ready(plain_extension_in(
                        access,
                        extension_defs,
                        &object,
                    ));
                };
                self.phase = RegionalCompositionPhase::WithSpec {
                    object,
                    spec: RegionalWhnfWork::from_focus(access, spec)
                        .with_source_owner(self.source_owner),
                    extension_defs: access.values().duplicate_value(extension_defs),
                };
                RegionalBuiltinPoll::Yielded
            }
            RegionalCompositionPhase::WithSpec {
                object,
                spec,
                extension_defs,
            } => {
                let spec = match poll_regional_whnf_in(access, spec, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                match finish_object_extension_in(access, object, &spec, extension_defs) {
                    Ok(value) => RegionalBuiltinPoll::Ready(value),
                    Err(message) => regional_failure(message),
                }
            }
            RegionalCompositionPhase::ComposedPriorBase {
                application,
                extension_defs,
                self_value,
            } => {
                let prior_stage = match poll_regional_whnf_in(access, application, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let prior_result = application_value_in(access, &prior_stage, &[self_value]);
                self.phase = RegionalCompositionPhase::ComposedExtensionPrior {
                    application: regional_application_in(
                        access,
                        access.values().duplicate_value(extension_defs),
                        &[prior_result],
                        self.source_owner,
                    ),
                    self_value: access.values().duplicate_value(self_value),
                };
                RegionalBuiltinPoll::Yielded
            }
            RegionalCompositionPhase::ComposedExtensionPrior {
                application,
                self_value,
            } => {
                let extension_stage = match poll_regional_whnf_in(access, application, step_budget)
                {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                RegionalBuiltinPoll::Ready(application_value_in(
                    access,
                    &extension_stage,
                    &[self_value],
                ))
            }
            RegionalCompositionPhase::OverrideUpdates { updates, base } => {
                let updates = match poll_regional_whnf_in(access, updates, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                self.phase = RegionalCompositionPhase::OverrideBase {
                    updates,
                    base: RegionalWhnfWork::from_focus(
                        access,
                        access.values().duplicate_value(base),
                    )
                    .with_source_owner(self.source_owner),
                };
                RegionalBuiltinPoll::Yielded
            }
            RegionalCompositionPhase::OverrideBase { updates, base } => {
                let base = match poll_regional_whnf_in(access, base, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let machine = match RegionalObjectOverrideMachine::new_in(
                    access,
                    base,
                    access.values().duplicate_value(updates),
                    self.source_owner,
                ) {
                    Ok(machine) => machine,
                    Err(message) => return regional_failure(message),
                };
                self.phase = RegionalCompositionPhase::Override(machine);
                RegionalBuiltinPoll::Yielded
            }
            RegionalCompositionPhase::Override(machine) => machine.poll_in(access, step_budget),
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.phase {
            RegionalCompositionPhase::WithObject {
                object,
                extension_defs,
            } => {
                object.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(extension_defs, visitor);
            }
            RegionalCompositionPhase::WithSpec {
                object,
                spec,
                extension_defs,
            } => {
                for value in [object, extension_defs] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
                spec.trace_managed_edges(visitor);
            }
            RegionalCompositionPhase::ComposedPriorBase {
                application,
                extension_defs,
                self_value,
            } => {
                application.trace_managed_edges(visitor);
                for value in [extension_defs, self_value] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
            }
            RegionalCompositionPhase::ComposedExtensionPrior {
                application,
                self_value,
            } => {
                application.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(self_value, visitor);
            }
            RegionalCompositionPhase::OverrideUpdates { updates, base } => {
                updates.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(base, visitor);
            }
            RegionalCompositionPhase::OverrideBase { updates, base } => {
                trace_compatibility_value_managed_edges(updates, visitor);
                base.trace_managed_edges(visitor);
            }
            RegionalCompositionPhase::Override(machine) => machine.trace_managed_edges(visitor),
        }
    }
}

impl RegionalObjectOverrideMachine {
    fn new_in(
        access: &EvaluationValueAccess<'_>,
        base: Value,
        updates: Value,
        source_owner: LazyId,
    ) -> Result<Self, &'static str> {
        let frame = regional_override_frame_in(access, base, updates, None)?;
        Ok(Self {
            stack: vec![frame],
            source_owner,
        })
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        let pending = self
            .stack
            .last_mut()
            .expect("unfinished object override retains a frame")
            .pending
            .take();
        if let Some(mut pending) = pending {
            let prior = match poll_regional_whnf_in(access, &mut pending.prior, step_budget) {
                Ok(value) => value,
                Err(poll) => {
                    self.stack
                        .last_mut()
                        .expect("pending object override retains its frame")
                        .pending = Some(pending);
                    return poll;
                }
            };
            if let Some(child) = regional_nested_override_frame_in(
                access,
                prior,
                access.values().duplicate_value(&pending.update),
                pending.key.clone(),
            ) {
                self.stack.push(child);
            } else {
                let result = {
                    let frame = self
                        .stack
                        .last()
                        .expect("pending object override retains its frame");
                    insert_regional_override_value_in(
                        access,
                        &frame.result,
                        pending.key,
                        &pending.update,
                    )
                };
                self.stack
                    .last_mut()
                    .expect("pending object override retains its frame")
                    .result = result;
            }
            return RegionalBuiltinPoll::Yielded;
        }

        let complete = {
            let frame = self
                .stack
                .last()
                .expect("unfinished object override retains a frame");
            frame.next == frame.keys.len()
        };
        if complete {
            let completed = self
                .stack
                .pop()
                .expect("completed object override retains a frame");
            let Some(parent) = self.stack.last_mut() else {
                return RegionalBuiltinPoll::Ready(completed.result);
            };
            let key = completed
                .return_key
                .expect("a nested object override retains its parent key");
            parent.result =
                insert_regional_override_value_in(access, &parent.result, key, &completed.result);
            return RegionalBuiltinPoll::Yielded;
        }

        let step = {
            let frame = self
                .stack
                .last_mut()
                .expect("unfinished object override retains a frame");
            let key = frame.keys[frame.next].clone();
            frame.next += 1;
            next_regional_override_step_in(access, &frame.result, &frame.updates, key)
        };
        match step {
            RegionalOverrideStep::Inserted(result) => {
                self.stack
                    .last_mut()
                    .expect("unfinished object override retains a frame")
                    .result = result;
            }
            RegionalOverrideStep::DemandPrior { key, update, prior } => {
                self.stack
                    .last_mut()
                    .expect("unfinished object override retains a frame")
                    .pending = Some(RegionalPendingOverride {
                    key,
                    update,
                    prior: RegionalWhnfWork::from_focus(access, prior)
                        .with_source_owner(self.source_owner),
                });
            }
        }
        RegionalBuiltinPoll::Yielded
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for frame in &self.stack {
            for value in [&frame.result, &frame.updates] {
                trace_compatibility_value_managed_edges(value, visitor);
            }
            if let Some(pending) = &frame.pending {
                trace_compatibility_value_managed_edges(&pending.update, visitor);
                pending.prior.trace_managed_edges(visitor);
            }
        }
    }
}

enum RegionalOverrideStep {
    Inserted(Value),
    DemandPrior {
        key: Key,
        update: Value,
        prior: Value,
    },
}

fn regional_override_frame_in(
    _access: &EvaluationValueAccess<'_>,
    result: Value,
    updates: Value,
    return_key: Option<Key>,
) -> Result<RegionalOverrideFrame, &'static str> {
    if !matches!(result, Value::Dict(_)) {
        return Err("object override definitions require dictionary values");
    }
    let Value::Dict(update_values) = &updates else {
        return Err("object override definitions require dictionary values");
    };
    let keys = update_values.iter().map(|(key, _)| key.clone()).collect();
    Ok(RegionalOverrideFrame {
        result,
        updates,
        keys,
        next: 0,
        pending: None,
        return_key,
    })
}

fn regional_nested_override_frame_in(
    access: &EvaluationValueAccess<'_>,
    prior: Value,
    updates: Value,
    return_key: Key,
) -> Option<RegionalOverrideFrame> {
    matches!(prior, Value::Dict(_))
        .then(|| regional_override_frame_in(access, prior, updates, Some(return_key)))
        .transpose()
        .expect("nested override updates are already known dictionaries")
}

fn next_regional_override_step_in(
    access: &EvaluationValueAccess<'_>,
    result: &Value,
    updates: &Value,
    key: Key,
) -> RegionalOverrideStep {
    let Value::Dict(result_values) = result else {
        unreachable!("object override result remains a dictionary")
    };
    let Value::Dict(update_values) = updates else {
        unreachable!("object override updates remain a dictionary")
    };
    let update = update_values
        .get(&key)
        .expect("an inventoried object override key remains present");
    if let (Some(prior), Value::Dict(_)) = (result_values.get(&key), update) {
        return RegionalOverrideStep::DemandPrior {
            key,
            update: access.values().duplicate_value(update),
            prior: access.values().duplicate_value(prior),
        };
    }
    let result = result_values.insert(key, access.values().duplicate_value(update));
    RegionalOverrideStep::Inserted(Value::Dict(result))
}

fn insert_regional_override_value_in(
    access: &EvaluationValueAccess<'_>,
    result: &Value,
    key: Key,
    value: &Value,
) -> Value {
    let Value::Dict(result) = result else {
        unreachable!("object override result remains a dictionary")
    };
    Value::Dict(result.insert(key, access.values().duplicate_value(value)))
}

fn poll_regional_whnf_in(
    access: &EvaluationValueAccess<'_>,
    computation: &mut RegionalWhnfWork,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Result<Value, RegionalBuiltinPoll> {
    match drive_regional_in_place(access, computation, step_budget, reduce_semantic_shell) {
        RegionalWhnfStatus::Ready(value) => Ok(value),
        RegionalWhnfStatus::Boundary(request) => Err(RegionalBuiltinPoll::Boundary(request)),
        RegionalWhnfStatus::Yielded => Err(RegionalBuiltinPoll::Yielded),
        RegionalWhnfStatus::Failed(failure) => Err(RegionalBuiltinPoll::Failed(failure)),
    }
}

fn regional_application_in(
    access: &EvaluationValueAccess<'_>,
    function: Value,
    arguments: &[Value],
    source_owner: LazyId,
) -> RegionalWhnfWork {
    RegionalWhnfWork::from_application_checkpoint_in(
        access,
        function,
        arguments,
        Some(source_owner),
    )
}

fn application_value_in(
    access: &EvaluationValueAccess<'_>,
    function: &Value,
    arguments: &[&Value],
) -> Value {
    Value::Lazy(LazyValue::from_application_in(
        access.values(),
        access.values().duplicate_value(function),
        Arc::from(
            arguments
                .iter()
                .map(|argument| access.values().duplicate_value(argument))
                .collect::<Vec<_>>(),
        ),
    ))
}

fn plain_extension_in(
    access: &EvaluationValueAccess<'_>,
    extension_defs: &Value,
    object: &Value,
) -> Value {
    let extension = LazyValue::from_application_in(
        access.values(),
        access.values().duplicate_value(extension_defs),
        Arc::from([access.values().duplicate_value(object)]),
    );
    Value::Lazy(LazyValue::from_builtin_in(
        access.values(),
        BuiltinCall {
            builtin: Builtin::Fixpoint,
            arguments: Arc::from([Value::Lazy(extension)]),
        },
    ))
}

fn finish_object_extension_in(
    access: &EvaluationValueAccess<'_>,
    object: &Value,
    spec: &Value,
    extension_defs: &Value,
) -> Result<Value, &'static str> {
    let Value::Dict(spec) = spec else {
        return Err("object instance builtin requires a specification dictionary");
    };
    if spec.is_empty() {
        return Ok(plain_extension_in(access, extension_defs, object));
    }
    let name = spec
        .get(&*keys::NAME)
        .map(|value| access.values().duplicate_value(value))
        .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
    let deps = spec
        .get(&*keys::DEPS)
        .map(|value| access.values().duplicate_value(value))
        .unwrap_or_else(|| Value::List(List::empty()));
    let prior_defs = spec
        .get(&*keys::DEFS)
        .map(|value| access.values().duplicate_value(value))
        .unwrap_or(Value::Builtin(Builtin::ObjectDefaultDefs));
    let composed_defs = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::ObjectComposedDefs,
        arguments: Arc::from([prior_defs, access.values().duplicate_value(extension_defs)]),
    });
    let spec = Value::Dict(
        Dict::new_sync()
            .insert((*keys::NAME).clone(), name)
            .insert((*keys::DEPS).clone(), deps)
            .insert((*keys::DEFS).clone(), composed_defs),
    );
    Ok(Value::Lazy(LazyValue::computed_fixpoint_in(
        access.values(),
        "object self",
        FixpointComputation::ObjectInstance(spec),
    )))
}

fn regional_failure(message: impl Into<Arc<str>>) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message.into())))
}
