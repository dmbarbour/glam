//! Durable object-composition construction.
//!
//! Ordinary object extension owns only the object/specification demand. The
//! callable chain itself is represented by ordinary lazy applications so its
//! existing WHNF owner preserves exact progress across suspension.

use std::sync::Arc;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluationFailure, FixpointComputation, LazyValue, List, Value,
    keys,
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
}

impl ObjectCompositionMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::ObjectWithDefs | Builtin::ObjectComposedDefs
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
        }
    }
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
