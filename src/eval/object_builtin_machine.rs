//! Durable object inspection and local-name construction.

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
use super::dict_machine::merge_dicts_in;
use super::list_machine::{ListFrontMachine, ListFrontPoll};
use super::whnf::WhnfComputation;

pub(crate) struct ObjectBuiltinMachine {
    phase: ObjectPhase,
}

enum ObjectPhase {
    SpecObject {
        object: WhnfComputation,
    },
    SpecValue {
        spec: WhnfComputation,
    },
    DiagnosticMessage {
        message: WhnfComputation,
    },
    DiagnosticSpec {
        message: RuntimeValueRoot,
        spec: WhnfComputation,
    },
    LocalHost {
        host: WhnfComputation,
        parts: RuntimeValueRoot,
    },
    LocalSpec {
        spec: WhnfComputation,
        parts: RuntimeValueRoot,
    },
    LocalName {
        name: WhnfComputation,
        parts: RuntimeValueRoot,
    },
    LocalParts {
        name: RuntimeValueRoot,
        parts: WhnfComputation,
    },
    LocalPartsFront {
        values: Vec<RuntimeValueRoot>,
        front: ListFrontMachine,
    },
    Instance {
        spec: RuntimeValueRoot,
    },
    InstanceFromParts {
        name: RuntimeValueRoot,
        deps: RuntimeValueRoot,
        defs: RuntimeValueRoot,
    },
    DefaultDefs {
        base: WhnfComputation,
    },
    DictDefsBase {
        dict: RuntimeValueRoot,
        base: WhnfComputation,
    },
    DictDefsDict {
        base: RuntimeValueRoot,
        dict: WhnfComputation,
    },
}

impl ObjectBuiltinMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::ObjectSpec
                | Builtin::ObjectLocalName
                | Builtin::DiagnosticObject
                | Builtin::ObjectInstance
                | Builtin::ObjectInstanceFromParts
                | Builtin::ObjectDefaultDefs
                | Builtin::ObjectDictDefs
        )
    }

    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let phase = match builtin {
            Builtin::ObjectSpec => {
                let [object]: [RuntimeValueRoot; 1] = arguments
                    .try_into()
                    .expect("object spec retains one operand");
                ObjectPhase::SpecObject {
                    object: WhnfComputation::from_root(object),
                }
            }
            Builtin::DiagnosticObject => {
                let [message]: [RuntimeValueRoot; 1] = arguments
                    .try_into()
                    .expect("diagnostic object retains one operand");
                ObjectPhase::DiagnosticMessage {
                    message: WhnfComputation::from_root(message),
                }
            }
            Builtin::ObjectLocalName => {
                let [host, parts]: [RuntimeValueRoot; 2] = arguments
                    .try_into()
                    .expect("object local name retains two operands");
                ObjectPhase::LocalHost {
                    host: WhnfComputation::from_root(host),
                    parts,
                }
            }
            Builtin::ObjectInstance => {
                let [spec]: [RuntimeValueRoot; 1] = arguments
                    .try_into()
                    .expect("object instance retains one specification");
                ObjectPhase::Instance { spec }
            }
            Builtin::ObjectInstanceFromParts => {
                let [name, deps, defs]: [RuntimeValueRoot; 3] = arguments
                    .try_into()
                    .expect("parts-based object instance retains three fields");
                ObjectPhase::InstanceFromParts { name, deps, defs }
            }
            Builtin::ObjectDefaultDefs => {
                let [base, _self_value]: [RuntimeValueRoot; 2] = arguments
                    .try_into()
                    .expect("default object definitions retain two operands");
                ObjectPhase::DefaultDefs {
                    base: WhnfComputation::from_root(base),
                }
            }
            Builtin::ObjectDictDefs => {
                let [dict, base, _self_value]: [RuntimeValueRoot; 3] = arguments
                    .try_into()
                    .expect("dictionary object definitions retain three operands");
                ObjectPhase::DictDefsBase {
                    dict,
                    base: WhnfComputation::from_root(base),
                }
            }
            _ => unreachable!("object builtin machine received another builtin"),
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
            ObjectPhase::SpecObject { object } => {
                let object = match poll_whnf(object, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let spec = match object_spec_member(context, &object) {
                    Ok(spec) => spec,
                    Err(failure) => return BuiltinTaskPoll::Failed(failure),
                };
                self.phase = ObjectPhase::SpecValue {
                    spec: WhnfComputation::from_root(spec),
                };
                BuiltinTaskPoll::Yielded
            }
            ObjectPhase::SpecValue { spec } => {
                let spec = match poll_whnf(spec, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                finish_object_spec(context, spec)
            }
            ObjectPhase::DiagnosticMessage { message } => {
                let message = match poll_whnf(message, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let spec = match optional_spec_member(
                    context,
                    &message,
                    "object_from_dict requires a dictionary value",
                ) {
                    Ok(spec) => spec,
                    Err(failure) => return BuiltinTaskPoll::Failed(failure),
                };
                let Some(spec) = spec else {
                    return BuiltinTaskPoll::Ready(root_object_from_dict(context, &message));
                };
                self.phase = ObjectPhase::DiagnosticSpec {
                    message,
                    spec: WhnfComputation::from_root(spec),
                };
                BuiltinTaskPoll::Yielded
            }
            ObjectPhase::DiagnosticSpec { message, spec } => {
                let spec = match poll_whnf(spec, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                if is_undefined(context, &spec) {
                    BuiltinTaskPoll::Ready(root_object_from_dict(context, message))
                } else {
                    BuiltinTaskPoll::Ready(message.clone())
                }
            }
            ObjectPhase::LocalHost { host, parts } => {
                let host = match poll_whnf(host, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let spec = match object_spec_member(context, &host) {
                    Ok(spec) => spec,
                    Err(failure) => return BuiltinTaskPoll::Failed(failure),
                };
                self.phase = ObjectPhase::LocalSpec {
                    spec: WhnfComputation::from_root(spec),
                    parts: parts.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            ObjectPhase::LocalSpec { spec, parts } => {
                let spec = match poll_whnf(spec, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let name = match spec_name(context, &spec) {
                    Ok(name) => name,
                    Err(failure) => return BuiltinTaskPoll::Failed(failure),
                };
                self.phase = ObjectPhase::LocalName {
                    name: WhnfComputation::from_root(name),
                    parts: parts.clone(),
                };
                BuiltinTaskPoll::Yielded
            }
            ObjectPhase::LocalName { name, parts } => {
                let name = match poll_whnf(name, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                self.phase = ObjectPhase::LocalParts {
                    name,
                    parts: WhnfComputation::from_root(parts.clone()),
                };
                BuiltinTaskPoll::Yielded
            }
            ObjectPhase::LocalParts { name, parts } => {
                let parts = match poll_whnf(parts, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                let kind = context.with_value_access(|access| match access.clone_root(&parts) {
                    Value::List(_) => Some(true),
                    Value::Dict(dict) if dict.is_empty() => Some(false),
                    _ => None,
                });
                match kind {
                    Some(true) => {
                        self.phase = ObjectPhase::LocalPartsFront {
                            values: vec![name.clone()],
                            front: ListFrontMachine::unowned(parts),
                        };
                        BuiltinTaskPoll::Yielded
                    }
                    Some(false) => root_local_name(context, std::slice::from_ref(name)),
                    None => BuiltinTaskPoll::Failed(root_message(
                        context,
                        "object local name builtin requires a list of name parts",
                    )),
                }
            }
            ObjectPhase::LocalPartsFront { values, front } => {
                match front.poll(poll_context, context, durable_context, step_budget) {
                    ListFrontPoll::Ready(Some((value, tail))) => {
                        values.push(value);
                        *front = ListFrontMachine::unowned(tail);
                        BuiltinTaskPoll::Yielded
                    }
                    ListFrontPoll::Ready(None) => root_local_name(context, values),
                    ListFrontPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
                    ListFrontPoll::Yielded => BuiltinTaskPoll::Yielded,
                    ListFrontPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
                }
            }
            ObjectPhase::Instance { spec } => BuiltinTaskPoll::Ready(root_object_instance(
                context,
                ObjectInstanceInput::Spec(spec),
            )),
            ObjectPhase::InstanceFromParts { name, deps, defs } => BuiltinTaskPoll::Ready(
                root_object_instance(context, ObjectInstanceInput::Parts { name, deps, defs }),
            ),
            ObjectPhase::DefaultDefs { base } => {
                match poll_whnf(base, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => BuiltinTaskPoll::Ready(value),
                    DemandResult::Pending(poll) => poll,
                }
            }
            ObjectPhase::DictDefsBase { dict, base } => {
                let base = match poll_whnf(base, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                self.phase = ObjectPhase::DictDefsDict {
                    base,
                    dict: WhnfComputation::from_root(dict.clone()),
                };
                BuiltinTaskPoll::Yielded
            }
            ObjectPhase::DictDefsDict { base, dict } => {
                let dict = match poll_whnf(dict, poll_context, durable_context, step_budget) {
                    DemandResult::Ready(value) => value,
                    DemandResult::Pending(poll) => return poll,
                };
                finish_dict_defs(context, base, &dict)
            }
        }
    }
}

fn finish_dict_defs(
    context: &EvaluatorStepContext<'_>,
    base: &RuntimeValueRoot,
    dict: &RuntimeValueRoot,
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        let Value::Dict(base) = access.clone_root(base) else {
            return Err("dictionary union requires dictionary values");
        };
        let Value::Dict(dict) = access.clone_root(dict) else {
            return Err("dictionary union requires dictionary values");
        };
        Ok(access
            .values()
            .root_runtime_value(Value::Dict(merge_dicts_in(access.values(), &base, &dict))))
    });
    match result {
        Ok(value) => BuiltinTaskPoll::Ready(value),
        Err(message) => BuiltinTaskPoll::Failed(root_message(context, message)),
    }
}

enum ObjectInstanceInput<'a> {
    Spec(&'a RuntimeValueRoot),
    Parts {
        name: &'a RuntimeValueRoot,
        deps: &'a RuntimeValueRoot,
        defs: &'a RuntimeValueRoot,
    },
}

fn root_object_instance(
    context: &EvaluatorStepContext<'_>,
    input: ObjectInstanceInput<'_>,
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let spec = match input {
            ObjectInstanceInput::Spec(spec) => access.clone_root(spec),
            ObjectInstanceInput::Parts { name, deps, defs } => Value::Dict(
                Dict::new_sync()
                    .insert((*keys::NAME).clone(), access.clone_root(name))
                    .insert((*keys::DEPS).clone(), access.clone_root(deps))
                    .insert((*keys::DEFS).clone(), access.clone_root(defs)),
            ),
        };
        let object = LazyValue::computed_fixpoint_in(
            access.values(),
            "object self",
            FixpointComputation::ObjectInstance(spec),
        );
        access.values().root_runtime_value(Value::Lazy(object))
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
            unreachable!("object builtin produced an external {boundary:?} boundary")
        }
    }
}

fn object_spec_member(
    context: &EvaluatorStepContext<'_>,
    object: &RuntimeValueRoot,
) -> Result<RuntimeValueRoot, RuntimeFailureRoot> {
    let member = optional_spec_member(
        context,
        object,
        "object spec builtin requires an object value",
    )?;
    member.ok_or_else(|| {
        root_message(
            context,
            "object value requires a defined `spec`; use `object_from_dict` to convert a dictionary",
        )
    })
}

fn optional_spec_member(
    context: &EvaluatorStepContext<'_>,
    value: &RuntimeValueRoot,
    wrong_kind_message: &'static str,
) -> Result<Option<RuntimeValueRoot>, RuntimeFailureRoot> {
    context.with_value_access(|access| {
        let Value::Dict(dict) = access.clone_root(value) else {
            return Err(root_message(context, wrong_kind_message));
        };
        Ok(dict.get(&*keys::SPEC).map(|spec| {
            access
                .values()
                .root_runtime_value(access.values().duplicate_value(spec))
        }))
    })
}

fn finish_object_spec(
    context: &EvaluatorStepContext<'_>,
    spec: RuntimeValueRoot,
) -> BuiltinTaskPoll {
    context.with_value_access(|access| match access.clone_root(&spec) {
        Value::Dict(dict) if dict.is_empty() => BuiltinTaskPoll::Failed(root_message(
            context,
            "object value requires a defined `spec`; use `object_from_dict` to convert a dictionary",
        )),
        Value::Dict(_) => BuiltinTaskPoll::Ready(spec),
        _ => BuiltinTaskPoll::Failed(root_message(
            context,
            "object value requires a dictionary-valued `spec`",
        )),
    })
}

fn spec_name(
    context: &EvaluatorStepContext<'_>,
    spec: &RuntimeValueRoot,
) -> Result<RuntimeValueRoot, RuntimeFailureRoot> {
    context.with_value_access(|access| {
        let Value::Dict(spec) = access.clone_root(spec) else {
            return Err(root_message(
                context,
                "object instance builtin requires a specification dictionary",
            ));
        };
        Ok(access.values().root_runtime_value(
            spec.get(&*keys::NAME)
                .map(|value| access.values().duplicate_value(value))
                .unwrap_or_else(|| Value::Dict(Dict::new_sync())),
        ))
    })
}

fn is_undefined(context: &EvaluatorStepContext<'_>, value: &RuntimeValueRoot) -> bool {
    context.with_value_access(
        |access| matches!(access.clone_root(value), Value::Dict(dict) if dict.is_empty()),
    )
}

fn root_object_from_dict(
    context: &EvaluatorStepContext<'_>,
    value: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let lazy = LazyValue::from_builtin_in(
            access.values(),
            BuiltinCall {
                builtin: Builtin::ObjectFromDict,
                arguments: Arc::from([access.clone_root(value)]),
            },
        );
        access.values().root_runtime_value(Value::Lazy(lazy))
    })
}

fn root_local_name(
    context: &EvaluatorStepContext<'_>,
    values: &[RuntimeValueRoot],
) -> BuiltinTaskPoll {
    BuiltinTaskPoll::Ready(context.with_value_access(|access| {
        let values = values
            .iter()
            .map(|value| access.clone_root(value))
            .collect();
        access
            .values()
            .root_runtime_value(Value::List(List::from_values(values)))
    }))
}

fn root_message(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<Arc<str>>,
) -> RuntimeFailureRoot {
    context.root_failure(Arc::new(EvaluationFailure::message(message.into())))
}
