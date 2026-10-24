//! Regional object inspection and local-name construction.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Builtin, BuiltinCall, Dict, EvaluationFailure, FixpointComputation, LazyId, LazyValue, List,
    Value, keys, trace_compatibility_value_managed_edges,
};
use crate::evaluation::EvaluationValueAccess;

use super::builtin_machine::RegionalBuiltinPoll;
use super::dict_machine::union_dicts_in;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalObjectBuiltinMachine {
    phase: ObjectPhase,
    source_owner: LazyId,
}

enum ObjectPhase {
    SpecObject {
        object: RegionalWhnfWork,
    },
    SpecValue {
        spec: RegionalWhnfWork,
    },
    DiagnosticMessage {
        message: RegionalWhnfWork,
    },
    DiagnosticSpec {
        message: Value,
        spec: RegionalWhnfWork,
    },
    /// `frames` is a list, and `prior` and `interface` dictionaries.
    ContextDefsPrior {
        frames: Value,
        prior: RegionalWhnfWork,
    },
    ContextDefsInterface {
        frames: Value,
        prior: Value,
        interface: RegionalWhnfWork,
    },
    ContextDefsContext {
        frames: Value,
        prior: Value,
        interface: Value,
        context: RegionalWhnfWork,
    },
    LocalHost {
        host: RegionalWhnfWork,
        parts: Value,
    },
    LocalSpec {
        spec: RegionalWhnfWork,
        parts: Value,
    },
    LocalName {
        name: RegionalWhnfWork,
        parts: Value,
    },
    LocalParts {
        name: Value,
        parts: RegionalWhnfWork,
    },
    LocalPartsFront {
        values: Vec<Value>,
        front: RegionalListFront,
    },
    Instance {
        spec: Value,
    },
    InstanceFromParts {
        name: Value,
        deps: Value,
        defs: Value,
    },
    DefaultDefs {
        base: RegionalWhnfWork,
    },
    DictDefsBase {
        dict: Value,
        base: RegionalWhnfWork,
    },
    DictDefsDict {
        base: Value,
        dict: RegionalWhnfWork,
    },
    FromDictValue {
        value: RegionalWhnfWork,
    },
    FromDictSpec {
        value: Value,
        spec: RegionalWhnfWork,
    },
}

impl RegionalObjectBuiltinMachine {
    pub(in crate::eval) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::ObjectSpec
                | Builtin::ObjectLocalName
                | Builtin::DiagnosticObject
                | Builtin::DiagnosticContextDefs
                | Builtin::ObjectInstance
                | Builtin::ObjectInstanceFromParts
                | Builtin::ObjectDefaultDefs
                | Builtin::ObjectDictDefs
                | Builtin::ObjectFromDict
        )
    }

    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let duplicate = |value: &Value| access.values().duplicate_value(value);
        let demand = |value: &Value| {
            RegionalWhnfWork::from_focus(access, duplicate(value)).with_source_owner(source_owner)
        };
        let phase = match builtin {
            Builtin::ObjectSpec => {
                let [object] = arguments else {
                    unreachable!("object spec retains one operand")
                };
                ObjectPhase::SpecObject {
                    object: demand(object),
                }
            }
            Builtin::DiagnosticObject => {
                let [message] = arguments else {
                    unreachable!("diagnostic object retains one operand")
                };
                ObjectPhase::DiagnosticMessage {
                    message: demand(message),
                }
            }
            Builtin::DiagnosticContextDefs => {
                let [frames, prior, _final] = arguments else {
                    unreachable!("diagnostic context definitions retain three operands")
                };
                assert!(
                    matches!(frames, Value::List(_)),
                    "diagnostic context frames are a list their sender built"
                );
                ObjectPhase::ContextDefsPrior {
                    frames: duplicate(frames),
                    prior: demand(prior),
                }
            }
            Builtin::ObjectLocalName => {
                let [host, parts] = arguments else {
                    unreachable!("object local name retains two operands")
                };
                ObjectPhase::LocalHost {
                    host: demand(host),
                    parts: duplicate(parts),
                }
            }
            Builtin::ObjectInstance => {
                let [spec] = arguments else {
                    unreachable!("object instance retains one specification")
                };
                ObjectPhase::Instance {
                    spec: duplicate(spec),
                }
            }
            Builtin::ObjectInstanceFromParts => {
                let [name, deps, defs] = arguments else {
                    unreachable!("parts-based object instance retains three fields")
                };
                ObjectPhase::InstanceFromParts {
                    name: duplicate(name),
                    deps: duplicate(deps),
                    defs: duplicate(defs),
                }
            }
            Builtin::ObjectDefaultDefs => {
                let [base, _self_value] = arguments else {
                    unreachable!("default object definitions retain two operands")
                };
                ObjectPhase::DefaultDefs { base: demand(base) }
            }
            Builtin::ObjectDictDefs => {
                let [dict, base, _self_value] = arguments else {
                    unreachable!("dictionary object definitions retain three operands")
                };
                ObjectPhase::DictDefsBase {
                    dict: duplicate(dict),
                    base: demand(base),
                }
            }
            Builtin::ObjectFromDict => {
                let [value] = arguments else {
                    unreachable!("object-from-dictionary retains one operand")
                };
                ObjectPhase::FromDictValue {
                    value: demand(value),
                }
            }
            _ => unreachable!("object builtin machine received another builtin"),
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
        let source_owner = self.source_owner;
        match &mut self.phase {
            ObjectPhase::SpecObject { object } => {
                let object = match poll_whnf_in(access, object, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let spec = match object_spec_member_in(access, &object) {
                    Ok(spec) => spec,
                    Err(failure) => return RegionalBuiltinPoll::Failed(failure),
                };
                self.phase = ObjectPhase::SpecValue {
                    spec: regional_demand(access, spec, source_owner),
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::SpecValue { spec } => {
                let spec = match poll_whnf_in(access, spec, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                finish_object_spec_in(access, spec)
            }
            ObjectPhase::DiagnosticMessage { message } => {
                let message = match poll_whnf_in(access, message, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let spec = match optional_spec_member_in(
                    access,
                    &message,
                    "object_from_dict requires a dictionary value",
                ) {
                    Ok(spec) => spec,
                    Err(failure) => return RegionalBuiltinPoll::Failed(failure),
                };
                let Some(spec) = spec else {
                    return RegionalBuiltinPoll::Ready(object_from_dict_in(access, &message));
                };
                self.phase = ObjectPhase::DiagnosticSpec {
                    message,
                    spec: regional_demand(access, spec, source_owner),
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::DiagnosticSpec { message, spec } => {
                let spec = match poll_whnf_in(access, spec, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                if is_undefined_in(access, &spec) {
                    RegionalBuiltinPoll::Ready(object_from_dict_in(access, message))
                } else {
                    RegionalBuiltinPoll::Ready(access.values().duplicate_value(message))
                }
            }
            ObjectPhase::ContextDefsPrior { frames, prior } => {
                let prior = match poll_whnf_in(access, prior, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let Value::Dict(definitions) = &prior else {
                    return failure("diagnostic context requires dictionary definitions");
                };
                let Some(interface) = definitions.get(&*keys::MSG) else {
                    let interface = Value::Dict(Dict::new_sync());
                    return finish_context_defs_in(access, frames, &prior, &interface, None);
                };
                self.phase = ObjectPhase::ContextDefsInterface {
                    frames: access.values().duplicate_value(frames),
                    interface: regional_demand(
                        access,
                        access.values().duplicate_value(interface),
                        source_owner,
                    ),
                    prior,
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::ContextDefsInterface {
                frames,
                prior,
                interface,
            } => {
                let interface = match poll_whnf_in(access, interface, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let Value::Dict(members) = &interface else {
                    return failure("a diagnostic's `msg` interface must be a dictionary");
                };
                let Some(context) = members.get(&*keys::CONTEXT) else {
                    return finish_context_defs_in(access, frames, prior, &interface, None);
                };
                self.phase = ObjectPhase::ContextDefsContext {
                    frames: access.values().duplicate_value(frames),
                    prior: access.values().duplicate_value(prior),
                    context: regional_demand(
                        access,
                        access.values().duplicate_value(context),
                        source_owner,
                    ),
                    interface,
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::ContextDefsContext {
                frames,
                prior,
                interface,
                context,
            } => {
                let context = match poll_whnf_in(access, context, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                finish_context_defs_in(access, frames, prior, interface, Some(&context))
            }
            ObjectPhase::LocalHost { host, parts } => {
                let host = match poll_whnf_in(access, host, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let spec = match object_spec_member_in(access, &host) {
                    Ok(spec) => spec,
                    Err(failure) => return RegionalBuiltinPoll::Failed(failure),
                };
                self.phase = ObjectPhase::LocalSpec {
                    spec: regional_demand(access, spec, source_owner),
                    parts: access.values().duplicate_value(parts),
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::LocalSpec { spec, parts } => {
                let spec = match poll_whnf_in(access, spec, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let name = match spec_name_in(access, &spec) {
                    Ok(name) => name,
                    Err(failure) => return RegionalBuiltinPoll::Failed(failure),
                };
                self.phase = ObjectPhase::LocalName {
                    name: regional_demand(access, name, source_owner),
                    parts: access.values().duplicate_value(parts),
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::LocalName { name, parts } => {
                let name = match poll_whnf_in(access, name, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                self.phase = ObjectPhase::LocalParts {
                    name,
                    parts: regional_demand(
                        access,
                        access.values().duplicate_value(parts),
                        source_owner,
                    ),
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::LocalParts { name, parts } => {
                let parts = match poll_whnf_in(access, parts, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                match &parts {
                    Value::List(_) => {
                        self.phase = ObjectPhase::LocalPartsFront {
                            values: vec![access.values().duplicate_value(name)],
                            front: RegionalListFront::new_in(
                                access,
                                parts,
                                Some(self.source_owner),
                            ),
                        };
                        RegionalBuiltinPoll::Continue
                    }
                    Value::Dict(dict) if dict.is_empty() => RegionalBuiltinPoll::Ready(
                        local_name_in(access, std::slice::from_ref(name)),
                    ),
                    _ => failure("object local name builtin requires a list of name parts"),
                }
            }
            ObjectPhase::LocalPartsFront { values, front } => {
                match front.poll_in(access, step_budget) {
                    RegionalListFrontPoll::Ready(Some((value, tail))) => {
                        values.push(value);
                        *front = RegionalListFront::new_in(access, tail, Some(self.source_owner));
                        RegionalBuiltinPoll::Continue
                    }
                    RegionalListFrontPoll::Ready(None) => {
                        RegionalBuiltinPoll::Ready(local_name_in(access, values))
                    }
                    RegionalListFrontPoll::Boundary(request) => {
                        RegionalBuiltinPoll::Boundary(request)
                    }
                    RegionalListFrontPoll::Continue => RegionalBuiltinPoll::Continue,
                    RegionalListFrontPoll::BudgetExhausted => RegionalBuiltinPoll::BudgetExhausted,
                    RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
                }
            }
            ObjectPhase::Instance { spec } => RegionalBuiltinPoll::Ready(object_instance_in(
                access,
                ObjectInstanceInput::Spec(spec),
            )),
            ObjectPhase::InstanceFromParts { name, deps, defs } => RegionalBuiltinPoll::Ready(
                object_instance_in(access, ObjectInstanceInput::Parts { name, deps, defs }),
            ),
            ObjectPhase::DefaultDefs { base } => match poll_whnf_in(access, base, step_budget) {
                Ok(value) => RegionalBuiltinPoll::Ready(value),
                Err(poll) => poll,
            },
            ObjectPhase::DictDefsBase { dict, base } => {
                let base = match poll_whnf_in(access, base, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                self.phase = ObjectPhase::DictDefsDict {
                    base,
                    dict: regional_demand(
                        access,
                        access.values().duplicate_value(dict),
                        source_owner,
                    ),
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::DictDefsDict { base, dict } => {
                let dict = match poll_whnf_in(access, dict, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                finish_dict_defs_in(access, base, &dict)
            }
            ObjectPhase::FromDictValue { value } => {
                let value = match poll_whnf_in(access, value, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let spec = match optional_spec_member_in(
                    access,
                    &value,
                    "object_from_dict requires a dictionary value",
                ) {
                    Ok(spec) => spec,
                    Err(failure) => return RegionalBuiltinPoll::Failed(failure),
                };
                let Some(spec) = spec else {
                    return RegionalBuiltinPoll::Ready(instance_from_plain_dict_in(access, &value));
                };
                self.phase = ObjectPhase::FromDictSpec {
                    value,
                    spec: regional_demand(access, spec, source_owner),
                };
                RegionalBuiltinPoll::Continue
            }
            ObjectPhase::FromDictSpec { value, spec } => {
                let spec = match poll_whnf_in(access, spec, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                if is_undefined_in(access, &spec) {
                    RegionalBuiltinPoll::Ready(instance_from_plain_dict_in(access, value))
                } else {
                    failure("object_from_dict requires a plain dictionary, not an object")
                }
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.phase.trace_managed_edges(visitor);
    }
}

impl ObjectPhase {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match self {
            Self::SpecObject { object }
            | Self::SpecValue { spec: object }
            | Self::DiagnosticMessage { message: object }
            | Self::DefaultDefs { base: object }
            | Self::FromDictValue { value: object } => object.trace_managed_edges(visitor),
            Self::DiagnosticSpec { message, spec }
            | Self::FromDictSpec {
                value: message,
                spec,
            } => {
                trace_compatibility_value_managed_edges(message, visitor);
                spec.trace_managed_edges(visitor);
            }
            Self::ContextDefsPrior { frames, prior } => {
                trace_compatibility_value_managed_edges(frames, visitor);
                prior.trace_managed_edges(visitor);
            }
            Self::ContextDefsInterface {
                frames,
                prior,
                interface,
            } => {
                trace_compatibility_value_managed_edges(frames, visitor);
                trace_compatibility_value_managed_edges(prior, visitor);
                interface.trace_managed_edges(visitor);
            }
            Self::ContextDefsContext {
                frames,
                prior,
                interface,
                context,
            } => {
                for value in [frames, prior, interface] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
                context.trace_managed_edges(visitor);
            }
            Self::LocalHost { host, parts } => {
                host.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(parts, visitor);
            }
            Self::LocalSpec { spec, parts } | Self::LocalName { name: spec, parts } => {
                spec.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(parts, visitor);
            }
            Self::LocalParts { name, parts } => {
                trace_compatibility_value_managed_edges(name, visitor);
                parts.trace_managed_edges(visitor);
            }
            Self::LocalPartsFront { values, front } => {
                for value in values {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
                front.trace_managed_edges(visitor);
            }
            Self::Instance { spec } => trace_compatibility_value_managed_edges(spec, visitor),
            Self::InstanceFromParts { name, deps, defs } => {
                for value in [name, deps, defs] {
                    trace_compatibility_value_managed_edges(value, visitor);
                }
            }
            Self::DictDefsBase { dict, base } => {
                trace_compatibility_value_managed_edges(dict, visitor);
                base.trace_managed_edges(visitor);
            }
            Self::DictDefsDict { base, dict } => {
                trace_compatibility_value_managed_edges(base, visitor);
                dict.trace_managed_edges(visitor);
            }
        }
    }
}

fn poll_whnf_in(
    access: &EvaluationValueAccess<'_>,
    computation: &mut RegionalWhnfWork,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Result<Value, RegionalBuiltinPoll> {
    match drive_regional_in_place(access, computation, step_budget, reduce_semantic_shell) {
        RegionalWhnfStatus::Ready(value) => Ok(value),
        RegionalWhnfStatus::Boundary(request) => Err(RegionalBuiltinPoll::Boundary(request)),
        RegionalWhnfStatus::BudgetExhausted => Err(RegionalBuiltinPoll::BudgetExhausted),
        RegionalWhnfStatus::Failed(failure) => Err(RegionalBuiltinPoll::Failed(failure)),
    }
}

fn regional_demand(
    access: &EvaluationValueAccess<'_>,
    value: Value,
    source_owner: LazyId,
) -> RegionalWhnfWork {
    RegionalWhnfWork::from_focus(access, value).with_source_owner(source_owner)
}

/// `prior` with `frames` prepended to its `msg.context`: a list context
/// follows the frames, an absent or undefined one contributes nothing, and
/// any other value is one frame.
fn finish_context_defs_in(
    access: &EvaluationValueAccess<'_>,
    frames: &Value,
    prior: &Value,
    interface: &Value,
    context: Option<&Value>,
) -> RegionalBuiltinPoll {
    let (Value::List(frames), Value::Dict(prior), Value::Dict(interface)) =
        (frames, prior, interface)
    else {
        unreachable!("diagnostic context phases hold a frame list and dictionaries")
    };
    let existing = match context {
        Some(Value::List(context)) => context.clone(),
        Some(context) if is_undefined_in(access, context) => List::empty(),
        Some(context) => List::from_values(vec![access.values().duplicate_value(context)]),
        None => List::empty(),
    };
    let interface = interface.insert(
        (*keys::CONTEXT).clone(),
        Value::List(List::concat(frames.clone(), existing)),
    );
    RegionalBuiltinPoll::Ready(Value::Dict(
        prior.insert((*keys::MSG).clone(), Value::Dict(interface)),
    ))
}

fn finish_dict_defs_in(
    access: &EvaluationValueAccess<'_>,
    base: &Value,
    dict: &Value,
) -> RegionalBuiltinPoll {
    let Value::Dict(base) = base else {
        return failure("dictionary union requires dictionary values");
    };
    let Value::Dict(dict) = dict else {
        return failure("dictionary union requires dictionary values");
    };
    RegionalBuiltinPoll::Ready(Value::Dict(union_dicts_in(access.values(), base, dict)))
}

enum ObjectInstanceInput<'a> {
    Spec(&'a Value),
    Parts {
        name: &'a Value,
        deps: &'a Value,
        defs: &'a Value,
    },
}

fn object_instance_in(access: &EvaluationValueAccess<'_>, input: ObjectInstanceInput<'_>) -> Value {
    let spec = match input {
        ObjectInstanceInput::Spec(spec) => access.values().duplicate_value(spec),
        ObjectInstanceInput::Parts { name, deps, defs } => Value::Dict(
            Dict::new_sync()
                .insert((*keys::NAME).clone(), access.values().duplicate_value(name))
                .insert((*keys::DEPS).clone(), access.values().duplicate_value(deps))
                .insert((*keys::DEFS).clone(), access.values().duplicate_value(defs)),
        ),
    };
    Value::Lazy(LazyValue::computed_fixpoint_in(
        access.values(),
        FixpointComputation::ObjectInstance(spec),
    ))
}

fn instance_from_plain_dict_in(access: &EvaluationValueAccess<'_>, value: &Value) -> Value {
    let Value::Dict(dict) = value else {
        unreachable!("plain-dictionary conversion retains a validated dictionary")
    };
    let defs = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::ObjectDictDefs,
        arguments: Arc::from([Value::Dict(dict.clone())]),
    });
    let spec = Value::Dict(
        Dict::new_sync()
            .insert((*keys::NAME).clone(), Value::Dict(Dict::new_sync()))
            .insert((*keys::DEPS).clone(), Value::List(List::empty()))
            .insert((*keys::DEFS).clone(), defs),
    );
    Value::Lazy(LazyValue::computed_fixpoint_in(
        access.values(),
        FixpointComputation::ObjectInstance(spec),
    ))
}

fn object_spec_member_in(
    access: &EvaluationValueAccess<'_>,
    object: &Value,
) -> Result<Value, Arc<EvaluationFailure>> {
    optional_spec_member_in(
        access,
        object,
        "object spec builtin requires an object value",
    )?
    .ok_or_else(|| {
        Arc::new(EvaluationFailure::message(
            "object value requires a defined `spec`; use `object_from_dict` to convert a dictionary",
        ))
    })
}

fn optional_spec_member_in(
    access: &EvaluationValueAccess<'_>,
    value: &Value,
    wrong_kind_message: &'static str,
) -> Result<Option<Value>, Arc<EvaluationFailure>> {
    let Value::Dict(dict) = value else {
        return Err(Arc::new(EvaluationFailure::message(wrong_kind_message)));
    };
    Ok(dict
        .get(&*keys::SPEC)
        .map(|spec| access.values().duplicate_value(spec)))
}

fn finish_object_spec_in(_access: &EvaluationValueAccess<'_>, spec: Value) -> RegionalBuiltinPoll {
    match spec {
        Value::Dict(dict) if dict.is_empty() => failure(
            "object value requires a defined `spec`; use `object_from_dict` to convert a dictionary",
        ),
        Value::Dict(_) => RegionalBuiltinPoll::Ready(spec),
        _ => failure("object value requires a dictionary-valued `spec`"),
    }
}

fn spec_name_in(
    access: &EvaluationValueAccess<'_>,
    spec: &Value,
) -> Result<Value, Arc<EvaluationFailure>> {
    let Value::Dict(spec) = spec else {
        return Err(Arc::new(EvaluationFailure::message(
            "object instance builtin requires a specification dictionary",
        )));
    };
    Ok(spec
        .get(&*keys::NAME)
        .map(|value| access.values().duplicate_value(value))
        .unwrap_or_else(|| Value::Dict(Dict::new_sync())))
}

fn is_undefined_in(_access: &EvaluationValueAccess<'_>, value: &Value) -> bool {
    matches!(value, Value::Dict(dict) if dict.is_empty())
}

fn object_from_dict_in(access: &EvaluationValueAccess<'_>, value: &Value) -> Value {
    Value::Lazy(LazyValue::from_builtin_in(
        access.values(),
        BuiltinCall {
            builtin: Builtin::ObjectFromDict,
            arguments: Arc::from([access.values().duplicate_value(value)]),
        },
    ))
}

fn local_name_in(access: &EvaluationValueAccess<'_>, values: &[Value]) -> Value {
    Value::List(List::from_values(
        values
            .iter()
            .map(|value| access.values().duplicate_value(value))
            .collect(),
    ))
}

fn failure(message: impl Into<Arc<str>>) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message.into())))
}
