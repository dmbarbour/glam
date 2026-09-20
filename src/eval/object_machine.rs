//! Pollable object-fixpoint construction.
//!
//! C3 traversal and the definitions fold retain one traced regional state. No
//! recursive evaluator call, registered root bundle, or Rust recursion
//! survives beneath that state.

use std::collections::BTreeMap;
use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Builtin, Dict, EvaluationFailure, EvaluationHalt, Key, LazyId, Value, keys,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::EvaluationValueAccess;

#[cfg(test)]
use crate::evaluation::EvalContext;

use super::access_machine::{RegionalConversionPoll, RegionalKeyConversion};
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place,
    reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalObjectFixpoint {
    source_owner: LazyId,
    original_spec: Value,
    self_marker: Value,
    state: ObjectState,
}

enum ObjectState {
    Linearize(ObjectLinearizationMachine),
    Mix(ObjectMixMachine),
}

struct ObjectLinearizationMachine {
    source_owner: LazyId,
    stack: Vec<LinearizationFrame>,
    seen: BTreeMap<Key, Value>,
    next_anonymous_id: u64,
}

struct LinearizationFrame {
    state: LinearizationState,
}

enum LinearizationState {
    DemandSpec(RegionalWhnfWork),
    ConvertName {
        spec: Value,
        conversion: RegionalKeyConversion,
    },
    DemandDeps {
        entry: LinearizedObjectSpec,
        demand: RegionalWhnfWork,
    },
    ReadDeps {
        entry: LinearizedObjectSpec,
        front: RegionalListFront,
        deps: Vec<Value>,
    },
    VisitDeps {
        entry: LinearizedObjectSpec,
        deps: Vec<Value>,
        next: usize,
        sequences: Vec<Vec<LinearizedObjectSpec>>,
        direct: Vec<LinearizedObjectSpec>,
        saw_named: bool,
        awaiting_child: bool,
    },
    Transition,
}

struct LinearizedObjectSpec {
    spec: Value,
    name: Key,
    anonymous_id: Option<u64>,
}

struct ObjectMixMachine {
    specs: Vec<Value>,
    next: usize,
    base: Value,
    pending_defs: Vec<Value>,
    phase: MixPhase,
    application: Option<RegionalWhnfWork>,
}

pub(in crate::eval) enum RegionalObjectFixpointPoll {
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

#[derive(Clone, Copy)]
enum MixPhase {
    Start,
    DemandDefs,
    ApplyBase,
    ApplySelf,
}

impl RegionalObjectFixpoint {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        spec: Value,
        self_marker: Value,
    ) -> Self {
        let original_spec = access.values().duplicate_value(&spec);
        Self {
            source_owner,
            original_spec,
            self_marker,
            state: ObjectState::Linearize(ObjectLinearizationMachine::new_in(
                access,
                spec,
                source_owner,
            )),
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalObjectFixpointPoll {
        match &mut self.state {
            ObjectState::Linearize(machine) => match machine.poll_in(access, step_budget) {
                LinearizationPoll::Ready(mut entries) => {
                    entries.reverse();
                    self.state = ObjectState::Mix(ObjectMixMachine::new_in(
                        access,
                        entries.into_iter().map(|entry| entry.spec).collect(),
                    ));
                    RegionalObjectFixpointPoll::Yielded
                }
                LinearizationPoll::Boundary(request) => {
                    RegionalObjectFixpointPoll::Boundary(request)
                }
                LinearizationPoll::Yielded => RegionalObjectFixpointPoll::Yielded,
                LinearizationPoll::Failed(failure) => RegionalObjectFixpointPoll::Failed(failure),
            },
            ObjectState::Mix(machine) => {
                match machine.poll_in(access, step_budget, self.source_owner, &self.self_marker) {
                    RegionalObjectFixpointPoll::Ready(base) => {
                        match finish_object_in(access, base, &self.original_spec) {
                            Ok(object) => RegionalObjectFixpointPoll::Ready(object),
                            Err(error) => {
                                RegionalObjectFixpointPoll::Failed(error.into_permanent_failure())
                            }
                        }
                    }
                    other => other,
                }
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.original_spec, visitor);
        trace_compatibility_value_managed_edges(&self.self_marker, visitor);
        self.state.trace_managed_edges(visitor);
    }
}

enum LinearizationPoll {
    Ready(Vec<LinearizedObjectSpec>),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

impl ObjectLinearizationMachine {
    fn new_in(access: &EvaluationValueAccess<'_>, spec: Value, source_owner: LazyId) -> Self {
        Self {
            source_owner,
            stack: vec![LinearizationFrame::new_in(access, spec, source_owner)],
            seen: BTreeMap::new(),
            next_anonymous_id: 0,
        }
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> LinearizationPoll {
        let state = std::mem::replace(
            &mut self
                .stack
                .last_mut()
                .expect("unfinished linearization retains a frame")
                .state,
            LinearizationState::Transition,
        );
        match state {
            LinearizationState::DemandSpec(mut demand) => match drive_regional_in_place(
                access,
                &mut demand,
                step_budget,
                reduce_semantic_shell,
            ) {
                RegionalWhnfStatus::Ready(spec) => {
                    let name =
                        match spec_member_in(access, &spec, &keys::NAME, SpecMemberDefault::Name) {
                            Ok(name) => name,
                            Err(error) => {
                                return LinearizationPoll::Failed(error.into_permanent_failure());
                            }
                        };
                    self.top_state(LinearizationState::ConvertName {
                        spec,
                        conversion: RegionalKeyConversion::new(
                            access,
                            name,
                            Some(self.source_owner),
                        ),
                    });
                    LinearizationPoll::Yielded
                }
                RegionalWhnfStatus::Boundary(request) => {
                    self.top_state(LinearizationState::DemandSpec(demand));
                    LinearizationPoll::Boundary(request)
                }
                RegionalWhnfStatus::Yielded => {
                    self.top_state(LinearizationState::DemandSpec(demand));
                    LinearizationPoll::Yielded
                }
                RegionalWhnfStatus::Failed(failure) => LinearizationPoll::Failed(failure),
            },
            LinearizationState::ConvertName {
                spec,
                mut conversion,
            } => match conversion.poll_in(access, step_budget) {
                RegionalConversionPoll::Ready(name) => {
                    let anonymous_id = if is_anonymous_object_name(&name) {
                        let id = self.next_anonymous_id;
                        self.next_anonymous_id += 1;
                        Some(id)
                    } else {
                        if let Err(error) =
                            remember_object_spec_in(access, &name, &spec, &mut self.seen)
                        {
                            return LinearizationPoll::Failed(error.into_permanent_failure());
                        }
                        None
                    };
                    let entry = LinearizedObjectSpec {
                        spec: access.values().duplicate_value(&spec),
                        name,
                        anonymous_id,
                    };
                    let deps = match spec_member_in(
                        access,
                        &spec,
                        &keys::DEPS,
                        SpecMemberDefault::Dependencies,
                    ) {
                        Ok(deps) => deps,
                        Err(error) => {
                            return LinearizationPoll::Failed(error.into_permanent_failure());
                        }
                    };
                    self.top_state(LinearizationState::DemandDeps {
                        entry,
                        demand: RegionalWhnfWork::from_focus(access, deps)
                            .with_source_owner(self.source_owner),
                    });
                    LinearizationPoll::Yielded
                }
                RegionalConversionPoll::Boundary(request) => {
                    self.top_state(LinearizationState::ConvertName { spec, conversion });
                    LinearizationPoll::Boundary(request)
                }
                RegionalConversionPoll::Yielded => {
                    self.top_state(LinearizationState::ConvertName { spec, conversion });
                    LinearizationPoll::Yielded
                }
                RegionalConversionPoll::Failed(failure) => LinearizationPoll::Failed(failure),
            },
            LinearizationState::DemandDeps { entry, mut demand } => {
                match drive_regional_in_place(
                    access,
                    &mut demand,
                    step_budget,
                    reduce_semantic_shell,
                ) {
                    RegionalWhnfStatus::Ready(deps) => match classify_deps_in(access, deps) {
                        Ok(Some(list)) => {
                            self.top_state(LinearizationState::ReadDeps {
                                entry,
                                front: RegionalListFront::new_in(
                                    access,
                                    list,
                                    Some(self.source_owner),
                                ),
                                deps: Vec::new(),
                            });
                            LinearizationPoll::Yielded
                        }
                        Ok(None) => {
                            self.top_state(empty_visit(entry));
                            LinearizationPoll::Yielded
                        }
                        Err(error) => LinearizationPoll::Failed(error.into_permanent_failure()),
                    },
                    RegionalWhnfStatus::Boundary(request) => {
                        self.top_state(LinearizationState::DemandDeps { entry, demand });
                        LinearizationPoll::Boundary(request)
                    }
                    RegionalWhnfStatus::Yielded => {
                        self.top_state(LinearizationState::DemandDeps { entry, demand });
                        LinearizationPoll::Yielded
                    }
                    RegionalWhnfStatus::Failed(failure) => LinearizationPoll::Failed(failure),
                }
            }
            LinearizationState::ReadDeps {
                entry,
                mut front,
                mut deps,
            } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(Some((value, tail))) => {
                    deps.push(value);
                    self.top_state(LinearizationState::ReadDeps {
                        entry,
                        front: RegionalListFront::new_in(access, tail, Some(self.source_owner)),
                        deps,
                    });
                    LinearizationPoll::Yielded
                }
                RegionalListFrontPoll::Ready(None) => {
                    self.top_state(LinearizationState::VisitDeps {
                        entry,
                        deps,
                        next: 0,
                        sequences: Vec::new(),
                        direct: Vec::new(),
                        saw_named: false,
                        awaiting_child: false,
                    });
                    LinearizationPoll::Yielded
                }
                RegionalListFrontPoll::Boundary(request) => {
                    self.top_state(LinearizationState::ReadDeps { entry, front, deps });
                    LinearizationPoll::Boundary(request)
                }
                RegionalListFrontPoll::Yielded => {
                    self.top_state(LinearizationState::ReadDeps { entry, front, deps });
                    LinearizationPoll::Yielded
                }
                RegionalListFrontPoll::Failed(failure) => LinearizationPoll::Failed(failure),
            },
            LinearizationState::VisitDeps {
                entry,
                deps,
                next,
                sequences,
                direct,
                saw_named,
                awaiting_child,
            } => {
                debug_assert!(
                    !awaiting_child,
                    "a child frame must remain above its parent"
                );
                let Some(dep) = deps
                    .get(next)
                    .map(|dep| access.values().duplicate_value(dep))
                else {
                    return self.finish_frame(access, entry, sequences, direct);
                };
                self.top_state(LinearizationState::VisitDeps {
                    entry,
                    deps,
                    next: next + 1,
                    sequences,
                    direct,
                    saw_named,
                    awaiting_child: true,
                });
                self.stack
                    .push(LinearizationFrame::new_in(access, dep, self.source_owner));
                LinearizationPoll::Yielded
            }
            LinearizationState::Transition => {
                unreachable!("object linearization transition must be replaced")
            }
        }
    }

    fn finish_frame(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        entry: LinearizedObjectSpec,
        mut sequences: Vec<Vec<LinearizedObjectSpec>>,
        direct: Vec<LinearizedObjectSpec>,
    ) -> LinearizationPoll {
        sequences.push(direct);
        let mut result = vec![entry];
        let merged = match c3_merge(access, sequences) {
            Ok(merged) => merged,
            Err(error) => return LinearizationPoll::Failed(error.into_permanent_failure()),
        };
        result.extend(merged);
        self.stack.pop();
        let Some(parent) = self.stack.last_mut() else {
            return LinearizationPoll::Ready(result);
        };
        let LinearizationState::VisitDeps {
            sequences,
            direct,
            saw_named,
            awaiting_child,
            ..
        } = &mut parent.state
        else {
            unreachable!("completed dependency must return to its visiting parent")
        };
        debug_assert!(*awaiting_child);
        let dep_entry = result
            .first()
            .map(|entry| entry.duplicate_in(access))
            .expect("object dependency linearization must retain its entry");
        if dep_entry.anonymous_id.is_some() {
            if *saw_named {
                return LinearizationPoll::Failed(
                    EvaluationHalt::new(
                        "anonymous object dependencies must appear before named object dependencies",
                    )
                    .into_permanent_failure(),
                );
            }
        } else {
            *saw_named = true;
        }
        direct.push(dep_entry);
        sequences.push(result);
        *awaiting_child = false;
        LinearizationPoll::Yielded
    }

    fn top_state(&mut self, state: LinearizationState) {
        self.stack
            .last_mut()
            .expect("unfinished linearization retains a frame")
            .state = state;
    }
}

impl LinearizationFrame {
    fn new_in(access: &EvaluationValueAccess<'_>, spec: Value, source_owner: LazyId) -> Self {
        Self {
            state: LinearizationState::DemandSpec(
                RegionalWhnfWork::from_focus(access, spec).with_source_owner(source_owner),
            ),
        }
    }
}

impl LinearizedObjectSpec {
    fn duplicate_in(&self, access: &EvaluationValueAccess<'_>) -> Self {
        Self {
            spec: access.values().duplicate_value(&self.spec),
            name: self.name.clone(),
            anonymous_id: self.anonymous_id,
        }
    }
}

impl ObjectMixMachine {
    fn new_in(_access: &EvaluationValueAccess<'_>, specs: Vec<Value>) -> Self {
        Self {
            specs,
            next: 0,
            base: Value::Dict(Dict::new_sync()),
            pending_defs: Vec::new(),
            phase: MixPhase::Start,
            application: None,
        }
    }

    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
        source_owner: LazyId,
        self_marker: &Value,
    ) -> RegionalObjectFixpointPoll {
        if let Some(application) = &mut self.application {
            let value = match drive_regional_in_place(
                access,
                application,
                step_budget,
                reduce_semantic_shell,
            ) {
                RegionalWhnfStatus::Ready(value) => value,
                RegionalWhnfStatus::Boundary(request) => {
                    return RegionalObjectFixpointPoll::Boundary(request);
                }
                RegionalWhnfStatus::Yielded => return RegionalObjectFixpointPoll::Yielded,
                RegionalWhnfStatus::Failed(failure) => {
                    return RegionalObjectFixpointPoll::Failed(failure);
                }
            };
            self.application = None;
            match self.phase {
                MixPhase::DemandDefs => {
                    if let Some((prior, extension)) = composed_defs_parts_in(access, &value) {
                        // This is a stack. The prior mixin must run before the
                        // extension, matching ObjectComposedDefs semantics.
                        self.pending_defs.push(extension);
                        self.pending_defs.push(prior);
                        self.phase = MixPhase::Start;
                        return RegionalObjectFixpointPoll::Yielded;
                    }
                    self.application = Some(regional_application_in(
                        access,
                        value,
                        std::slice::from_ref(&self.base),
                        source_owner,
                    ));
                    self.phase = MixPhase::ApplyBase;
                    return RegionalObjectFixpointPoll::Yielded;
                }
                MixPhase::ApplyBase => {
                    self.application = Some(regional_application_in(
                        access,
                        value,
                        std::slice::from_ref(self_marker),
                        source_owner,
                    ));
                    self.phase = MixPhase::ApplySelf;
                    return RegionalObjectFixpointPoll::Yielded;
                }
                MixPhase::ApplySelf => {
                    if !matches!(value, Value::Dict(_)) {
                        return RegionalObjectFixpointPoll::Failed(
                            EvaluationHalt::new(
                                "object definition mixin must produce a dictionary",
                            )
                            .into_permanent_failure(),
                        );
                    }
                    self.base = value;
                    self.phase = MixPhase::Start;
                    return RegionalObjectFixpointPoll::Yielded;
                }
                MixPhase::Start => unreachable!("object mix phase must name its application"),
            }
        }

        if self.pending_defs.is_empty() {
            let Some(spec) = self.specs.get(self.next) else {
                return RegionalObjectFixpointPoll::Ready(
                    access.values().duplicate_value(&self.base),
                );
            };
            let defs =
                match spec_member_in(access, spec, &keys::DEFS, SpecMemberDefault::Definitions) {
                    Ok(defs) => defs,
                    Err(error) => {
                        return RegionalObjectFixpointPoll::Failed(error.into_permanent_failure());
                    }
                };
            self.next += 1;
            self.pending_defs.push(defs);
        }
        let defs = self
            .pending_defs
            .pop()
            .expect("an unfinished object spec retains a definitions mixin");
        self.application =
            Some(RegionalWhnfWork::from_focus(access, defs).with_source_owner(source_owner));
        self.phase = MixPhase::DemandDefs;
        RegionalObjectFixpointPoll::Yielded
    }
}

fn composed_defs_parts_in(
    access: &EvaluationValueAccess<'_>,
    defs: &Value,
) -> Option<(Value, Value)> {
    let Value::PartialBuiltin(call) = defs else {
        return None;
    };
    if call.builtin != Builtin::ObjectComposedDefs || call.arguments.len() != 2 {
        return None;
    }
    Some((
        access.values().duplicate_value(&call.arguments[0]),
        access.values().duplicate_value(&call.arguments[1]),
    ))
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

enum SpecMemberDefault {
    Name,
    Dependencies,
    Definitions,
}

fn spec_member_in(
    access: &EvaluationValueAccess<'_>,
    spec: &Value,
    key: &Key,
    default: SpecMemberDefault,
) -> Result<Value, EvaluationHalt> {
    let Value::Dict(spec) = spec else {
        return Err(EvaluationHalt::new(
            "object instance builtin requires a specification dictionary",
        ));
    };
    Ok(spec
        .get(key)
        .map(|value| access.values().duplicate_value(value))
        .unwrap_or_else(|| match default {
            SpecMemberDefault::Name => Value::Dict(Dict::new_sync()),
            SpecMemberDefault::Dependencies => Value::List(crate::core::List::empty()),
            SpecMemberDefault::Definitions => Value::Builtin(Builtin::ObjectDefaultDefs),
        }))
}

fn classify_deps_in(
    _access: &EvaluationValueAccess<'_>,
    deps: Value,
) -> Result<Option<Value>, EvaluationHalt> {
    match deps {
        value @ Value::List(_) => Ok(Some(value)),
        Value::Dict(dict) if dict.is_empty() => Ok(None),
        _ => Err(EvaluationHalt::new(
            "object specification deps must evaluate to a list",
        )),
    }
}

fn empty_visit(entry: LinearizedObjectSpec) -> LinearizationState {
    LinearizationState::VisitDeps {
        entry,
        deps: Vec::new(),
        next: 0,
        sequences: Vec::new(),
        direct: Vec::new(),
        saw_named: false,
        awaiting_child: false,
    }
}

fn remember_object_spec_in(
    access: &EvaluationValueAccess<'_>,
    name: &Key,
    spec: &Value,
    seen: &mut BTreeMap<Key, Value>,
) -> Result<(), EvaluationHalt> {
    match seen.get(name) {
        Some(prior) if !same_spec_in(access, prior, spec) => Err(EvaluationHalt::new(format!(
            "object specification name {name:?} identifies multiple specifications"
        ))),
        Some(_) => Ok(()),
        None => {
            seen.insert(name.clone(), access.values().duplicate_value(spec));
            Ok(())
        }
    }
}

fn same_spec_in(_access: &EvaluationValueAccess<'_>, left: &Value, right: &Value) -> bool {
    let (Value::Dict(left), Value::Dict(right)) = (left, right) else {
        unreachable!("linearized object specs must retain dictionaries")
    };
    left.ptr_eq(right)
}

fn is_anonymous_object_name(name: &Key) -> bool {
    matches!(name, Key::Dict(entries) if entries.is_empty())
}

fn c3_merge(
    access: &EvaluationValueAccess<'_>,
    mut sequences: Vec<Vec<LinearizedObjectSpec>>,
) -> Result<Vec<LinearizedObjectSpec>, EvaluationHalt> {
    let mut result = Vec::new();
    loop {
        sequences.retain(|sequence| !sequence.is_empty());
        if sequences.is_empty() {
            return Ok(result);
        }
        let selected = sequences.iter().find_map(|sequence| {
            let candidate = &sequence[0];
            (!sequences.iter().any(|other| {
                other
                    .iter()
                    .skip(1)
                    .any(|spec| same_linearized_object_spec(spec, candidate))
            }))
            .then(|| candidate.duplicate_in(access))
        });
        let Some(selected) = selected else {
            return Err(EvaluationHalt::new(
                "object dependencies have inconsistent C3 linearization",
            ));
        };
        result.push(selected.duplicate_in(access));
        for sequence in &mut sequences {
            if sequence
                .first()
                .is_some_and(|spec| same_linearized_object_spec(spec, &selected))
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

fn finish_object_in(
    access: &EvaluationValueAccess<'_>,
    base: Value,
    spec: &Value,
) -> Result<Value, EvaluationHalt> {
    let Value::Dict(base) = base else {
        return Err(EvaluationHalt::new("object base is not a dictionary"));
    };
    Ok(Value::Dict(base.insert(
        (*keys::SPEC).clone(),
        access.values().duplicate_value(spec),
    )))
}

impl ObjectState {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match self {
            Self::Linearize(machine) => machine.trace_managed_edges(visitor),
            Self::Mix(machine) => machine.trace_managed_edges(visitor),
        }
    }
}

impl ObjectLinearizationMachine {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for frame in &self.stack {
            frame.trace_managed_edges(visitor);
        }
        for spec in self.seen.values() {
            trace_compatibility_value_managed_edges(spec, visitor);
        }
    }
}

impl LinearizationFrame {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.state.trace_managed_edges(visitor);
    }
}

impl LinearizationState {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match self {
            Self::DemandSpec(work) => work.trace_managed_edges(visitor),
            Self::ConvertName { spec, conversion } => {
                trace_compatibility_value_managed_edges(spec, visitor);
                conversion.trace_managed_edges(visitor);
            }
            Self::DemandDeps { entry, demand } => {
                entry.trace_managed_edges(visitor);
                demand.trace_managed_edges(visitor);
            }
            Self::ReadDeps { entry, front, deps } => {
                entry.trace_managed_edges(visitor);
                front.trace_managed_edges(visitor);
                trace_object_values(deps, visitor);
            }
            Self::VisitDeps {
                entry,
                deps,
                sequences,
                direct,
                ..
            } => {
                entry.trace_managed_edges(visitor);
                trace_object_values(deps, visitor);
                for sequence in sequences {
                    for entry in sequence {
                        entry.trace_managed_edges(visitor);
                    }
                }
                for entry in direct {
                    entry.trace_managed_edges(visitor);
                }
            }
            Self::Transition => {}
        }
    }
}

impl LinearizedObjectSpec {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.spec, visitor);
    }
}

impl ObjectMixMachine {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_object_values(&self.specs, visitor);
        trace_compatibility_value_managed_edges(&self.base, visitor);
        trace_object_values(&self.pending_defs, visitor);
        if let Some(application) = &self.application {
            application.trace_managed_edges(visitor);
        }
    }
}

fn trace_object_values(values: &[Value], visitor: &mut Visitor<'_>) {
    for value in values {
        trace_compatibility_value_managed_edges(value, visitor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CoreValueFactory, FixpointComputation, LazyValue, List, ListThunk, PromisedValue,
    };
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    use super::super::test_support::{TestExpr, closed_function_value_in};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    #[test]
    fn object_fixpoint_resumes_through_spec_name_and_dependency_chunk() {
        let context = context();
        let spec = PromisedValue::new(context.values(), "object spec");
        let name = PromisedValue::new(context.values(), "object name");
        let deps = PromisedValue::new(context.values(), "object dependency chunk");
        let object = Value::Lazy(LazyValue::computed_fixpoint(
            context.values(),
            "object self",
            FixpointComputation::ObjectInstance(Value::Promised(spec.clone())),
        ));

        crate::eval::eval_value(&context, &object)
            .expect_err("object construction must first wait for its spec");
        let spec_value = Value::Dict(
            Dict::new_sync()
                .insert((*keys::NAME).clone(), Value::Promised(name.clone()))
                .insert(
                    (*keys::DEPS).clone(),
                    Value::List(List::from_thunk(ListThunk::Promised(deps.clone()))),
                ),
        );
        crate::core::set_test_promise(context.values(), &spec, spec_value)
            .expect("the object spec promise should accept its assignment");

        crate::eval::eval_value(&context, &object)
            .expect_err("object construction must resume at its name");
        crate::core::set_test_promise(context.values(), &name, Value::binary_from_text("root"))
            .expect("the object name promise should accept its assignment");

        crate::eval::eval_value(&context, &object)
            .expect_err("object construction must resume at its dependency chunk");
        crate::core::set_test_promise(context.values(), &deps, Value::List(List::empty()))
            .expect("the dependency chunk should accept its assignment");

        let Value::Dict(result) =
            crate::eval::eval_value(&context, &object).expect("object construction should finish")
        else {
            panic!("object construction must produce a dictionary")
        };
        assert!(result.get(&*keys::SPEC).is_some());
    }

    #[test]
    fn object_fixpoint_resumes_through_a_nested_dependency_spec() {
        let context = context();
        let dependency = PromisedValue::new(context.values(), "nested object dependency");
        let root_spec = Value::Dict(
            Dict::new_sync()
                .insert((*keys::NAME).clone(), Value::binary_from_text("root"))
                .insert(
                    (*keys::DEPS).clone(),
                    Value::List(List::from_values(vec![Value::Promised(dependency.clone())])),
                ),
        );
        let object = Value::Lazy(LazyValue::computed_fixpoint(
            context.values(),
            "object self",
            FixpointComputation::ObjectInstance(root_spec),
        ));

        crate::eval::eval_value(&context, &object)
            .expect_err("linearization must wait for the nested dependency spec");
        crate::core::set_test_promise(
            context.values(),
            &dependency,
            Value::Dict(
                Dict::new_sync()
                    .insert((*keys::NAME).clone(), Value::binary_from_text("dependency"))
                    .insert((*keys::DEPS).clone(), Value::List(List::empty())),
            ),
        )
        .expect("the dependency spec should accept its assignment");

        let Value::Dict(result) =
            crate::eval::eval_value(&context, &object).expect("linearization should resume")
        else {
            panic!("object construction must produce a dictionary")
        };
        assert!(result.get(&*keys::SPEC).is_some());
    }

    #[test]
    fn object_fixpoint_resumes_each_mixin_application_once() {
        let context = context();
        let first_result = PromisedValue::new(context.values(), "first mixin application");
        let second_result = PromisedValue::new(context.values(), "second mixin application");
        let defs = closed_function_value_in(
            context.values(),
            1,
            TestExpr::Value(Value::Promised(first_result.clone())),
        );
        let spec = Value::Dict(
            Dict::new_sync()
                .insert((*keys::NAME).clone(), Value::binary_from_text("root"))
                .insert((*keys::DEPS).clone(), Value::List(List::empty()))
                .insert((*keys::DEFS).clone(), defs),
        );
        let object = Value::Lazy(LazyValue::computed_fixpoint(
            context.values(),
            "object self",
            FixpointComputation::ObjectInstance(spec),
        ));

        crate::eval::eval_value(&context, &object)
            .expect_err("the first definitions application must suspend");
        let second_stage = closed_function_value_in(
            context.values(),
            1,
            TestExpr::Value(Value::Promised(second_result.clone())),
        );
        crate::core::set_test_promise(context.values(), &first_result, second_stage)
            .expect("the first application should accept its function result");

        crate::eval::eval_value(&context, &object)
            .expect_err("the second definitions application must suspend");
        let expected =
            Dict::new_sync().insert(Key::binary_from_text("answer"), Value::Number(42.into()));
        crate::core::set_test_promise(
            context.values(),
            &second_result,
            Value::Dict(expected.clone()),
        )
        .expect("the second application should accept its dictionary result");

        let Value::Dict(result) =
            crate::eval::eval_value(&context, &object).expect("the definitions fold should resume")
        else {
            panic!("object construction must produce a dictionary")
        };
        assert_eq!(
            result.get(&Key::binary_from_text("answer")),
            expected.get(&Key::binary_from_text("answer"))
        );
        assert!(result.get(&*keys::SPEC).is_some());
    }

    #[test]
    fn object_fixpoint_resumes_through_composed_definition_function_calls() {
        let context = context();
        let expected =
            Dict::new_sync().insert(Key::binary_from_text("answer"), Value::Number(42.into()));
        let extension = closed_function_value_in(
            context.values(),
            2,
            TestExpr::Value(Value::Dict(expected.clone())),
        );
        let defs = Value::builtin_call(
            context.values(),
            Builtin::ObjectComposedDefs,
            vec![Value::Builtin(Builtin::ObjectDefaultDefs), extension],
        );
        let spec = Value::Dict(
            Dict::new_sync()
                .insert((*keys::NAME).clone(), Value::binary_from_text("root"))
                .insert((*keys::DEPS).clone(), Value::List(List::empty()))
                .insert((*keys::DEFS).clone(), defs),
        );
        let object = Value::Lazy(LazyValue::computed_fixpoint(
            context.values(),
            "object self",
            FixpointComputation::ObjectInstance(spec),
        ));

        let Value::Dict(result) = crate::eval::eval_value(&context, &object)
            .expect("composed definitions should resume after the extension function call")
        else {
            panic!("object construction must produce a dictionary")
        };
        assert_eq!(
            result.get(&Key::binary_from_text("answer")),
            expected.get(&Key::binary_from_text("answer"))
        );
        assert!(result.get(&*keys::SPEC).is_some());
    }
}
