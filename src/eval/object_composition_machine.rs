//! Durable object-composition construction.
//!
//! Ordinary object extension owns only the object/specification demand. The
//! callable chain itself is represented by ordinary lazy applications so its
//! existing WHNF owner preserves exact progress across suspension.

use std::sync::Arc;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluationFailure, FixpointComputation, Key, LazyValue, List,
    Value, keys,
};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::builtin_machine::BuiltinTaskPoll;
use super::whnf::WhnfComputation;

pub(crate) struct ObjectCompositionMachine {
    phase: CompositionPhase,
}

enum CompositionPhase {
    WithObject {
        object: WhnfComputation,
        extension_defs: RuntimeValueRoot,
    },
    WithSpec {
        object: RuntimeValueRoot,
        spec: WhnfComputation,
        extension_defs: RuntimeValueRoot,
    },
    Composed {
        prior_defs: RuntimeValueRoot,
        extension_defs: RuntimeValueRoot,
        base: RuntimeValueRoot,
        self_value: RuntimeValueRoot,
    },
    ComposedPriorBase {
        application: WhnfComputation,
        extension_defs: RuntimeValueRoot,
        self_value: RuntimeValueRoot,
    },
    ComposedExtensionPrior {
        application: WhnfComputation,
        self_value: RuntimeValueRoot,
    },
    OverrideUpdates {
        updates: WhnfComputation,
        base: RuntimeValueRoot,
    },
    OverrideBase {
        updates: RuntimeValueRoot,
        base: WhnfComputation,
    },
    Override(ObjectOverrideMachine),
}

struct ObjectOverrideMachine {
    stack: Vec<OverrideFrame>,
}

struct OverrideFrame {
    result: RuntimeValueRoot,
    updates: RuntimeValueRoot,
    keys: Vec<Key>,
    next: usize,
    pending: Option<PendingOverride>,
    return_key: Option<Key>,
}

struct PendingOverride {
    key: Key,
    update: RuntimeValueRoot,
    prior: WhnfComputation,
}

impl ObjectCompositionMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::ObjectWithDefs | Builtin::ObjectComposedDefs | Builtin::ObjectOverrideDefs
        )
    }

    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let phase = match builtin {
            Builtin::ObjectWithDefs => {
                let [object, extension_defs]: [RuntimeValueRoot; 2] = arguments
                    .try_into()
                    .expect("object extension retains two operands");
                CompositionPhase::WithObject {
                    object: WhnfComputation::from_root(object),
                    extension_defs,
                }
            }
            Builtin::ObjectComposedDefs => {
                let [prior_defs, extension_defs, base, self_value]: [RuntimeValueRoot; 4] =
                    arguments
                        .try_into()
                        .expect("composed object definitions retain four operands");
                CompositionPhase::Composed {
                    prior_defs,
                    extension_defs,
                    base,
                    self_value,
                }
            }
            Builtin::ObjectOverrideDefs => {
                let [updates, base, _self_value]: [RuntimeValueRoot; 3] = arguments
                    .try_into()
                    .expect("object override definitions retain three operands");
                CompositionPhase::OverrideUpdates {
                    updates: WhnfComputation::from_root(updates),
                    base,
                }
            }
            _ => unreachable!("object composition machine received another builtin"),
        };
        Self { phase }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        match &mut self.phase {
            CompositionPhase::WithObject {
                object,
                extension_defs,
            } => {
                let object = match poll_whnf(object, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let spec = context.with_value_access(|access| {
                    let Value::Dict(object) = access.clone_root(&object) else {
                        return Err("ordinary `with` requires a dictionary or object value");
                    };
                    Ok(object.get(&*keys::SPEC).map(|spec| {
                        access
                            .values()
                            .root_runtime_value(access.values().duplicate_value(spec))
                    }))
                });
                let spec = match spec {
                    Ok(spec) => spec,
                    Err(message) => return BuiltinTaskPoll::Failed(root_message(context, message)),
                };
                let Some(spec) = spec else {
                    return BuiltinTaskPoll::Ready(root_plain_extension(
                        context,
                        extension_defs,
                        &object,
                    ));
                };
                self.phase = CompositionPhase::WithSpec {
                    object,
                    spec: WhnfComputation::from_root(spec),
                    extension_defs: extension_defs.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            CompositionPhase::WithSpec {
                object,
                spec,
                extension_defs,
            } => {
                let spec = match poll_whnf(spec, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                match finish_object_extension(context, object, &spec, extension_defs) {
                    Ok(value) => BuiltinTaskPoll::Ready(value),
                    Err(message) => BuiltinTaskPoll::Failed(root_message(context, message)),
                }
            }
            CompositionPhase::Composed {
                prior_defs,
                extension_defs,
                base,
                self_value,
            } => {
                self.phase = CompositionPhase::ComposedPriorBase {
                    application: application_in(context, prior_defs, std::slice::from_ref(base)),
                    extension_defs: extension_defs.clone(),
                    self_value: self_value.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            CompositionPhase::ComposedPriorBase {
                application,
                extension_defs,
                self_value,
            } => {
                let prior_stage =
                    match poll_whnf(application, poll_context, durable_context, step_budget) {
                        DemandResult::Ready(value) => value,
                        DemandResult::Pending(poll) => return poll,
                    };
                let prior_result =
                    root_application(context, &prior_stage, std::slice::from_ref(self_value));
                self.phase = CompositionPhase::ComposedExtensionPrior {
                    application: application_in(
                        context,
                        extension_defs,
                        std::slice::from_ref(&prior_result),
                    ),
                    self_value: self_value.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            CompositionPhase::ComposedExtensionPrior {
                application,
                self_value,
            } => {
                let extension_stage =
                    match poll_whnf(application, poll_context, durable_context, step_budget) {
                        DemandResult::Ready(value) => value,
                        DemandResult::Pending(poll) => return poll,
                    };
                BuiltinTaskPoll::Ready(root_application(
                    context,
                    &extension_stage,
                    std::slice::from_ref(self_value),
                ))
            }
            CompositionPhase::OverrideUpdates { updates, base } => {
                let updates = match poll_whnf(updates, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                self.phase = CompositionPhase::OverrideBase {
                    updates,
                    base: WhnfComputation::from_root(base.clone()),
                };
                BuiltinTaskPoll::Yielded
            }
            CompositionPhase::OverrideBase { updates, base } => {
                let base = match poll_whnf(base, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let machine = match ObjectOverrideMachine::new(context, base, updates.clone()) {
                    Ok(machine) => machine,
                    Err(message) => {
                        return BuiltinTaskPoll::Failed(root_message(context, message));
                    }
                };
                self.phase = CompositionPhase::Override(machine);
                BuiltinTaskPoll::Yielded
            }
            CompositionPhase::Override(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
        }
    }
}

impl ObjectOverrideMachine {
    fn new(
        context: &EvaluatorStepContext<'_>,
        base: RuntimeValueRoot,
        updates: RuntimeValueRoot,
    ) -> Result<Self, &'static str> {
        let frame = override_frame(context, base, updates, None)?;
        Ok(Self { stack: vec![frame] })
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        let pending = self
            .stack
            .last_mut()
            .expect("unfinished object override retains a frame")
            .pending
            .take();
        if let Some(mut pending) = pending {
            let prior = match poll_whnf(
                &mut pending.prior,
                poll_context,
                durable_context,
                step_budget,
            ) {
                DemandResult::Ready(value) => value,
                DemandResult::Pending(poll) => {
                    self.stack
                        .last_mut()
                        .expect("pending object override retains its frame")
                        .pending = Some(pending);
                    return poll;
                }
            };
            if let Some(child) =
                nested_override_frame(context, prior, pending.update.clone(), pending.key.clone())
            {
                self.stack.push(child);
            } else {
                let result = {
                    let frame = self
                        .stack
                        .last()
                        .expect("pending object override retains its frame");
                    insert_override_value(context, &frame.result, pending.key, &pending.update)
                };
                self.stack
                    .last_mut()
                    .expect("pending object override retains its frame")
                    .result = result;
            }
            return BuiltinTaskPoll::Yielded;
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
                return BuiltinTaskPoll::Ready(completed.result);
            };
            let key = completed
                .return_key
                .expect("a nested object override retains its parent key");
            parent.result = insert_override_value(context, &parent.result, key, &completed.result);
            return BuiltinTaskPoll::Yielded;
        }

        let step = {
            let frame = self
                .stack
                .last_mut()
                .expect("unfinished object override retains a frame");
            let key = frame.keys[frame.next].clone();
            frame.next += 1;
            next_override_step(context, &frame.result, &frame.updates, key)
        };
        match step {
            OverrideStep::Inserted(result) => {
                self.stack
                    .last_mut()
                    .expect("unfinished object override retains a frame")
                    .result = result;
            }
            OverrideStep::DemandPrior { key, update, prior } => {
                self.stack
                    .last_mut()
                    .expect("unfinished object override retains a frame")
                    .pending = Some(PendingOverride {
                    key,
                    update,
                    prior: WhnfComputation::from_root(prior),
                });
            }
        }
        BuiltinTaskPoll::Yielded
    }
}

enum OverrideStep {
    Inserted(RuntimeValueRoot),
    DemandPrior {
        key: Key,
        update: RuntimeValueRoot,
        prior: RuntimeValueRoot,
    },
}

fn override_frame(
    context: &EvaluatorStepContext<'_>,
    result: RuntimeValueRoot,
    updates: RuntimeValueRoot,
    return_key: Option<Key>,
) -> Result<OverrideFrame, &'static str> {
    let keys = context.with_value_access(|access| {
        if !matches!(access.clone_root(&result), Value::Dict(_)) {
            return Err("object override definitions require dictionary values");
        }
        let Value::Dict(update_values) = access.clone_root(&updates) else {
            return Err("object override definitions require dictionary values");
        };
        Ok(update_values.iter().map(|(key, _)| key.clone()).collect())
    })?;
    Ok(OverrideFrame {
        result,
        updates,
        keys,
        next: 0,
        pending: None,
        return_key,
    })
}

fn nested_override_frame(
    context: &EvaluatorStepContext<'_>,
    prior: RuntimeValueRoot,
    updates: RuntimeValueRoot,
    return_key: Key,
) -> Option<OverrideFrame> {
    let prior_is_dict =
        context.with_value_access(|access| matches!(access.clone_root(&prior), Value::Dict(_)));
    prior_is_dict
        .then(|| override_frame(context, prior, updates, Some(return_key)))
        .transpose()
        .expect("nested override updates are already known dictionaries")
}

fn next_override_step(
    context: &EvaluatorStepContext<'_>,
    result: &RuntimeValueRoot,
    updates: &RuntimeValueRoot,
    key: Key,
) -> OverrideStep {
    context.with_value_access(|access| {
        let Value::Dict(result_values) = access.clone_root(result) else {
            unreachable!("object override result remains a dictionary")
        };
        let Value::Dict(update_values) = access.clone_root(updates) else {
            unreachable!("object override updates remain a dictionary")
        };
        let update = update_values
            .get(&key)
            .expect("an inventoried object override key remains present");
        if let (Some(prior), Value::Dict(_)) = (result_values.get(&key), update) {
            return OverrideStep::DemandPrior {
                key,
                update: access
                    .values()
                    .root_runtime_value(access.values().duplicate_value(update)),
                prior: access
                    .values()
                    .root_runtime_value(access.values().duplicate_value(prior)),
            };
        }
        let result = result_values.insert(key, access.values().duplicate_value(update));
        OverrideStep::Inserted(access.values().root_runtime_value(Value::Dict(result)))
    })
}

fn insert_override_value(
    context: &EvaluatorStepContext<'_>,
    result: &RuntimeValueRoot,
    key: Key,
    value: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let Value::Dict(result) = access.clone_root(result) else {
            unreachable!("object override result remains a dictionary")
        };
        let result = result.insert(key, access.clone_root(value));
        access.values().root_runtime_value(Value::Dict(result))
    })
}

enum DemandResult {
    Ready(RuntimeValueRoot),
    Pending(BuiltinTaskPoll),
}

fn poll_whnf(
    computation: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    durable_context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> DemandResult {
    match poll_whnf_computation(computation, poll_context, durable_context, step_budget) {
        WhnfOwnerPoll::Ready(value) => DemandResult::Ready(value),
        WhnfOwnerPoll::Pending(dependency) => {
            DemandResult::Pending(BuiltinTaskPoll::Pending(dependency))
        }
        WhnfOwnerPoll::Yielded => DemandResult::Pending(BuiltinTaskPoll::Yielded),
        WhnfOwnerPoll::Failed(failure) => DemandResult::Pending(BuiltinTaskPoll::Failed(failure)),
        WhnfOwnerPoll::External(boundary) => {
            unreachable!("object composition produced an external {boundary:?} boundary")
        }
    }
}

fn root_plain_extension(
    context: &EvaluatorStepContext<'_>,
    extension_defs: &RuntimeValueRoot,
    object: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    context.with_value_access(|access| root_plain_extension_in(&access, extension_defs, object))
}

fn root_plain_extension_in(
    access: &crate::evaluation::EvaluationValueAccess<'_>,
    extension_defs: &RuntimeValueRoot,
    object: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    let extension = LazyValue::from_application_in(
        access.values(),
        access.clone_root(extension_defs),
        Arc::from([access.clone_root(object)]),
    );
    let fixed = LazyValue::from_builtin_in(
        access.values(),
        BuiltinCall {
            builtin: Builtin::Fixpoint,
            arguments: Arc::from([Value::Lazy(extension)]),
        },
    );
    access.values().root_runtime_value(Value::Lazy(fixed))
}

fn finish_object_extension(
    context: &EvaluatorStepContext<'_>,
    object: &RuntimeValueRoot,
    spec: &RuntimeValueRoot,
    extension_defs: &RuntimeValueRoot,
) -> Result<RuntimeValueRoot, &'static str> {
    context.with_value_access(|access| {
        let Value::Dict(spec) = access.clone_root(spec) else {
            return Err("object instance builtin requires a specification dictionary");
        };
        if spec.is_empty() {
            return Ok(root_plain_extension_in(&access, extension_defs, object));
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
            arguments: Arc::from([prior_defs, access.clone_root(extension_defs)]),
        });
        let spec = Value::Dict(
            Dict::new_sync()
                .insert((*keys::NAME).clone(), name)
                .insert((*keys::DEPS).clone(), deps)
                .insert((*keys::DEFS).clone(), composed_defs),
        );
        let object = LazyValue::computed_fixpoint_in(
            access.values(),
            "object self",
            FixpointComputation::ObjectInstance(spec),
        );
        Ok(access.values().root_runtime_value(Value::Lazy(object)))
    })
}

fn application_in(
    context: &EvaluatorStepContext<'_>,
    function: &RuntimeValueRoot,
    arguments: &[RuntimeValueRoot],
) -> WhnfComputation {
    context.with_value_access(|access| {
        let function = access.clone_root(function);
        let arguments = arguments
            .iter()
            .map(|argument| access.clone_root(argument))
            .collect::<Vec<_>>();
        WhnfComputation::from_application_checkpoint_in(&access, function, &arguments)
    })
}

fn root_application(
    context: &EvaluatorStepContext<'_>,
    function: &RuntimeValueRoot,
    arguments: &[RuntimeValueRoot],
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let arguments = arguments
            .iter()
            .map(|argument| access.clone_root(argument))
            .collect::<Vec<_>>();
        let application = LazyValue::from_application_in(
            access.values(),
            access.clone_root(function),
            Arc::from(arguments),
        );
        access.values().root_runtime_value(Value::Lazy(application))
    })
}

fn root_message(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<Arc<str>>,
) -> RuntimeFailureRoot {
    context.root_failure(Arc::new(EvaluationFailure::message(message.into())))
}
