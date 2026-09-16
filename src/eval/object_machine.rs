//! Pollable object-fixpoint construction.
//!
//! C3 traversal and the definitions fold retain explicit rooted state. No
//! recursive evaluator call or Rust recursion survives a returned poll.

use std::collections::BTreeMap;

use crate::core::{Builtin, Dict, EvaluationHalt, Key, LazyId, Value, keys};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::access_machine::{ConversionPoll, KeyConversionMachine};
use super::list_machine::{ListFrontMachine, ListFrontPoll};
use super::whnf::WhnfComputation;

pub(super) enum ObjectFixpointPoll {
    Ready(RuntimeValueRoot),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(super) struct ObjectFixpointMachine {
    source_owner: LazyId,
    original_spec: RuntimeValueRoot,
    self_marker: RuntimeValueRoot,
    state: ObjectState,
}

enum ObjectState {
    Linearize(ObjectLinearizationMachine),
    Mix(ObjectMixMachine),
}

struct ObjectLinearizationMachine {
    source_owner: LazyId,
    stack: Vec<LinearizationFrame>,
    seen: BTreeMap<Key, RuntimeValueRoot>,
    next_anonymous_id: u64,
}

struct LinearizationFrame {
    state: LinearizationState,
}

enum LinearizationState {
    DemandSpec(WhnfComputation),
    ConvertName {
        spec: RuntimeValueRoot,
        conversion: KeyConversionMachine,
    },
    DemandDeps {
        entry: LinearizedObjectSpec,
        demand: WhnfComputation,
    },
    ReadDeps {
        entry: LinearizedObjectSpec,
        tail: RuntimeValueRoot,
        front: Option<ListFrontMachine>,
        deps: Vec<RuntimeValueRoot>,
    },
    VisitDeps {
        entry: LinearizedObjectSpec,
        deps: Vec<RuntimeValueRoot>,
        next: usize,
        sequences: Vec<Vec<LinearizedObjectSpec>>,
        direct: Vec<LinearizedObjectSpec>,
        saw_named: bool,
        awaiting_child: bool,
    },
    Transition,
}

#[derive(Clone)]
struct LinearizedObjectSpec {
    spec: RuntimeValueRoot,
    name: Key,
    anonymous_id: Option<u64>,
}

struct ObjectMixMachine {
    specs: Vec<RuntimeValueRoot>,
    next: usize,
    base: RuntimeValueRoot,
    pending_defs: Vec<RuntimeValueRoot>,
    phase: MixPhase,
    application: Option<WhnfComputation>,
}

#[derive(Clone, Copy)]
enum MixPhase {
    Start,
    DemandDefs,
    ApplyBase,
    ApplySelf,
}

impl ObjectFixpointMachine {
    pub(super) fn new(
        source_owner: LazyId,
        spec: RuntimeValueRoot,
        self_marker: RuntimeValueRoot,
    ) -> Self {
        Self {
            source_owner,
            original_spec: spec.clone(),
            self_marker,
            state: ObjectState::Linearize(ObjectLinearizationMachine::new(spec, source_owner)),
        }
    }

    pub(super) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ObjectFixpointPoll {
        match &mut self.state {
            ObjectState::Linearize(machine) => {
                match machine.poll(poll_context, context, durable_context, step_budget) {
                    LinearizationPoll::Ready(mut entries) => {
                        entries.reverse();
                        self.state = ObjectState::Mix(ObjectMixMachine::new(
                            context,
                            entries.into_iter().map(|entry| entry.spec).collect(),
                        ));
                        ObjectFixpointPoll::Yielded
                    }
                    LinearizationPoll::Pending(dependency) => {
                        ObjectFixpointPoll::Pending(dependency)
                    }
                    LinearizationPoll::Yielded => ObjectFixpointPoll::Yielded,
                    LinearizationPoll::Failed(failure) => ObjectFixpointPoll::Failed(failure),
                }
            }
            ObjectState::Mix(machine) => match machine.poll(
                poll_context,
                context,
                durable_context,
                step_budget,
                self.source_owner,
                &self.self_marker,
            ) {
                ObjectFixpointPoll::Ready(base) => {
                    match finish_object(context, base, &self.original_spec) {
                        Ok(object) => ObjectFixpointPoll::Ready(object),
                        Err(error) => ObjectFixpointPoll::Failed(root_halt(context, error)),
                    }
                }
                other => other,
            },
        }
    }
}

enum LinearizationPoll {
    Ready(Vec<LinearizedObjectSpec>),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl ObjectLinearizationMachine {
    fn new(spec: RuntimeValueRoot, source_owner: LazyId) -> Self {
        Self {
            source_owner,
            stack: vec![LinearizationFrame::new(spec, source_owner)],
            seen: BTreeMap::new(),
            next_anonymous_id: 0,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
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
            LinearizationState::DemandSpec(mut demand) => {
                match poll_whnf_computation(&mut demand, poll_context, durable_context, step_budget)
                {
                    WhnfOwnerPoll::Ready(spec) => {
                        let name = match spec_name_root(context, &spec) {
                            Ok(name) => name,
                            Err(error) => {
                                return LinearizationPoll::Failed(root_halt(context, error));
                            }
                        };
                        self.top_state(LinearizationState::ConvertName {
                            spec,
                            conversion: KeyConversionMachine::new(name, Some(self.source_owner)),
                        });
                        LinearizationPoll::Yielded
                    }
                    WhnfOwnerPoll::Pending(dependency) => {
                        self.top_state(LinearizationState::DemandSpec(demand));
                        LinearizationPoll::Pending(dependency)
                    }
                    WhnfOwnerPoll::Yielded => {
                        self.top_state(LinearizationState::DemandSpec(demand));
                        LinearizationPoll::Yielded
                    }
                    WhnfOwnerPoll::Failed(failure) => LinearizationPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("object spec produced an external {boundary:?} boundary")
                    }
                }
            }
            LinearizationState::ConvertName {
                spec,
                mut conversion,
            } => match conversion.poll(poll_context, context, durable_context, step_budget) {
                ConversionPoll::Ready(name) => {
                    let anonymous_id = if is_anonymous_object_name(&name) {
                        let id = self.next_anonymous_id;
                        self.next_anonymous_id += 1;
                        Some(id)
                    } else {
                        if let Err(error) =
                            remember_object_spec(context, &name, &spec, &mut self.seen)
                        {
                            return LinearizationPoll::Failed(root_halt(context, error));
                        }
                        None
                    };
                    let entry = LinearizedObjectSpec {
                        spec: spec.clone(),
                        name,
                        anonymous_id,
                    };
                    let deps = match spec_member_root(context, &spec, &keys::DEPS, || {
                        Value::List(crate::core::List::empty())
                    }) {
                        Ok(deps) => deps,
                        Err(error) => return LinearizationPoll::Failed(root_halt(context, error)),
                    };
                    self.top_state(LinearizationState::DemandDeps {
                        entry,
                        demand: WhnfComputation::from_root(deps)
                            .with_source_owner(self.source_owner),
                    });
                    LinearizationPoll::Yielded
                }
                ConversionPoll::Pending(dependency) => {
                    self.top_state(LinearizationState::ConvertName { spec, conversion });
                    LinearizationPoll::Pending(dependency)
                }
                ConversionPoll::Yielded => {
                    self.top_state(LinearizationState::ConvertName { spec, conversion });
                    LinearizationPoll::Yielded
                }
                ConversionPoll::Failed(failure) => LinearizationPoll::Failed(failure),
            },
            LinearizationState::DemandDeps { entry, mut demand } => {
                match poll_whnf_computation(&mut demand, poll_context, durable_context, step_budget)
                {
                    WhnfOwnerPoll::Ready(deps) => match classify_deps(context, &deps) {
                        Ok(Some(list)) => {
                            self.top_state(LinearizationState::ReadDeps {
                                entry,
                                tail: list,
                                front: None,
                                deps: Vec::new(),
                            });
                            LinearizationPoll::Yielded
                        }
                        Ok(None) => {
                            self.top_state(empty_visit(entry));
                            LinearizationPoll::Yielded
                        }
                        Err(error) => LinearizationPoll::Failed(root_halt(context, error)),
                    },
                    WhnfOwnerPoll::Pending(dependency) => {
                        self.top_state(LinearizationState::DemandDeps { entry, demand });
                        LinearizationPoll::Pending(dependency)
                    }
                    WhnfOwnerPoll::Yielded => {
                        self.top_state(LinearizationState::DemandDeps { entry, demand });
                        LinearizationPoll::Yielded
                    }
                    WhnfOwnerPoll::Failed(failure) => LinearizationPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!(
                            "object dependencies produced an external {boundary:?} boundary"
                        )
                    }
                }
            }
            LinearizationState::ReadDeps {
                entry,
                tail,
                front,
                mut deps,
            } => {
                let mut front =
                    front.unwrap_or_else(|| ListFrontMachine::new(tail.clone(), self.source_owner));
                match front.poll(poll_context, context, durable_context, step_budget) {
                    ListFrontPoll::Ready(Some((value, tail))) => {
                        deps.push(value);
                        self.top_state(LinearizationState::ReadDeps {
                            entry,
                            tail,
                            front: None,
                            deps,
                        });
                        LinearizationPoll::Yielded
                    }
                    ListFrontPoll::Ready(None) => {
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
                    ListFrontPoll::Pending(dependency) => {
                        self.top_state(LinearizationState::ReadDeps {
                            entry,
                            tail,
                            front: Some(front),
                            deps,
                        });
                        LinearizationPoll::Pending(dependency)
                    }
                    ListFrontPoll::Yielded => {
                        self.top_state(LinearizationState::ReadDeps {
                            entry,
                            tail,
                            front: Some(front),
                            deps,
                        });
                        LinearizationPoll::Yielded
                    }
                    ListFrontPoll::Failed(failure) => LinearizationPoll::Failed(failure),
                }
            }
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
                let Some(dep) = deps.get(next).cloned() else {
                    return self.finish_frame(context, entry, sequences, direct);
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
                    .push(LinearizationFrame::new(dep, self.source_owner));
                LinearizationPoll::Yielded
            }
            LinearizationState::Transition => {
                unreachable!("object linearization transition must be replaced")
            }
        }
    }

    fn finish_frame(
        &mut self,
        context: &EvaluatorStepContext<'_>,
        entry: LinearizedObjectSpec,
        mut sequences: Vec<Vec<LinearizedObjectSpec>>,
        direct: Vec<LinearizedObjectSpec>,
    ) -> LinearizationPoll {
        sequences.push(direct);
        let mut result = vec![entry];
        let merged = match c3_merge(sequences) {
            Ok(merged) => merged,
            Err(error) => return LinearizationPoll::Failed(root_halt(context, error)),
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
            .cloned()
            .expect("object dependency linearization must retain its entry");
        if dep_entry.anonymous_id.is_some() {
            if *saw_named {
                return LinearizationPoll::Failed(root_halt(
                    context,
                    EvaluationHalt::new(
                        "anonymous object dependencies must appear before named object dependencies",
                    ),
                ));
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
    fn new(spec: RuntimeValueRoot, source_owner: LazyId) -> Self {
        Self {
            state: LinearizationState::DemandSpec(
                WhnfComputation::from_root(spec).with_source_owner(source_owner),
            ),
        }
    }
}

impl ObjectMixMachine {
    fn new(context: &EvaluatorStepContext<'_>, specs: Vec<RuntimeValueRoot>) -> Self {
        Self {
            specs,
            next: 0,
            base: context.root_value(Value::Dict(Dict::new_sync())),
            pending_defs: Vec::new(),
            phase: MixPhase::Start,
            application: None,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
        source_owner: LazyId,
        self_marker: &RuntimeValueRoot,
    ) -> ObjectFixpointPoll {
        if let Some(application) = &mut self.application {
            let value = match poll_whnf_computation(
                application,
                poll_context,
                durable_context,
                step_budget,
            ) {
                WhnfOwnerPoll::Ready(value) => value,
                WhnfOwnerPoll::Pending(dependency) => {
                    return ObjectFixpointPoll::Pending(dependency);
                }
                WhnfOwnerPoll::Yielded => return ObjectFixpointPoll::Yielded,
                WhnfOwnerPoll::Failed(failure) => return ObjectFixpointPoll::Failed(failure),
                WhnfOwnerPoll::External(boundary) => {
                    unreachable!("object mixin produced an external {boundary:?} boundary")
                }
            };
            self.application = None;
            match self.phase {
                MixPhase::DemandDefs => {
                    if let Some((prior, extension)) = composed_defs_parts(context, &value) {
                        // This is a stack. The prior mixin must run before the
                        // extension, matching ObjectComposedDefs semantics.
                        self.pending_defs.push(extension);
                        self.pending_defs.push(prior);
                        self.phase = MixPhase::Start;
                        return ObjectFixpointPoll::Yielded;
                    }
                    self.application = Some(application_in(
                        context,
                        value,
                        std::slice::from_ref(&self.base),
                        source_owner,
                    ));
                    self.phase = MixPhase::ApplyBase;
                    return ObjectFixpointPoll::Yielded;
                }
                MixPhase::ApplyBase => {
                    self.application = Some(application_in(
                        context,
                        value,
                        std::slice::from_ref(self_marker),
                        source_owner,
                    ));
                    self.phase = MixPhase::ApplySelf;
                    return ObjectFixpointPoll::Yielded;
                }
                MixPhase::ApplySelf => {
                    if !context.with_value_access(|access| {
                        matches!(access.clone_root(&value), Value::Dict(_))
                    }) {
                        return ObjectFixpointPoll::Failed(root_halt(
                            context,
                            EvaluationHalt::new(
                                "object definition mixin must produce a dictionary",
                            ),
                        ));
                    }
                    self.base = value;
                    self.phase = MixPhase::Start;
                    return ObjectFixpointPoll::Yielded;
                }
                MixPhase::Start => unreachable!("object mix phase must name its application"),
            }
        }

        if self.pending_defs.is_empty() {
            let Some(spec) = self.specs.get(self.next) else {
                return ObjectFixpointPoll::Ready(self.base.clone());
            };
            let defs = match spec_member_root(context, spec, &keys::DEFS, || {
                Value::Builtin(Builtin::ObjectDefaultDefs)
            }) {
                Ok(defs) => defs,
                Err(error) => return ObjectFixpointPoll::Failed(root_halt(context, error)),
            };
            self.next += 1;
            self.pending_defs.push(defs);
        }
        let defs = self
            .pending_defs
            .pop()
            .expect("an unfinished object spec retains a definitions mixin");
        self.application = Some(WhnfComputation::from_root(defs).with_source_owner(source_owner));
        self.phase = MixPhase::DemandDefs;
        ObjectFixpointPoll::Yielded
    }
}

fn composed_defs_parts(
    context: &EvaluatorStepContext<'_>,
    defs: &RuntimeValueRoot,
) -> Option<(RuntimeValueRoot, RuntimeValueRoot)> {
    context.with_value_access(|access| {
        let Value::PartialBuiltin(call) = access.clone_root(defs) else {
            return None;
        };
        if call.builtin != Builtin::ObjectComposedDefs || call.arguments.len() != 2 {
            return None;
        }
        Some((
            access
                .values()
                .root_runtime_value(access.values().duplicate_value(&call.arguments[0])),
            access
                .values()
                .root_runtime_value(access.values().duplicate_value(&call.arguments[1])),
        ))
    })
}

fn application_in(
    context: &EvaluatorStepContext<'_>,
    function: RuntimeValueRoot,
    arguments: &[RuntimeValueRoot],
    source_owner: LazyId,
) -> WhnfComputation {
    context.with_value_access(|access| {
        let function = access.clone_root(&function);
        let arguments = arguments
            .iter()
            .map(|argument| access.clone_root(argument))
            .collect::<Vec<_>>();
        WhnfComputation::from_application_checkpoint_in(&access, function, &arguments)
            .with_source_owner(source_owner)
    })
}

fn spec_name_root(
    context: &EvaluatorStepContext<'_>,
    spec: &RuntimeValueRoot,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    spec_member_root(context, spec, &keys::NAME, || Value::Dict(Dict::new_sync()))
}

fn spec_member_root(
    context: &EvaluatorStepContext<'_>,
    spec: &RuntimeValueRoot,
    key: &Key,
    default: impl FnOnce() -> Value,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    context.with_value_access(|access| {
        let Value::Dict(spec) = access.clone_root(spec) else {
            return Err(EvaluationHalt::new(
                "object instance builtin requires a specification dictionary",
            ));
        };
        Ok(access.values().root_runtime_value(
            spec.get(key)
                .map(|value| access.values().duplicate_value(value))
                .unwrap_or_else(default),
        ))
    })
}

fn classify_deps(
    context: &EvaluatorStepContext<'_>,
    deps: &RuntimeValueRoot,
) -> Result<Option<RuntimeValueRoot>, EvaluationHalt> {
    context.with_value_access(|access| match access.clone_root(deps) {
        Value::List(_) => Ok(Some(deps.clone())),
        Value::Dict(dict) if dict.is_empty() => Ok(None),
        _ => Err(EvaluationHalt::new(
            "object specification deps must evaluate to a list",
        )),
    })
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

fn remember_object_spec(
    context: &EvaluatorStepContext<'_>,
    name: &Key,
    spec: &RuntimeValueRoot,
    seen: &mut BTreeMap<Key, RuntimeValueRoot>,
) -> Result<(), EvaluationHalt> {
    match seen.get(name) {
        Some(prior) if !same_spec(context, prior, spec) => Err(EvaluationHalt::new(format!(
            "object specification name {name:?} identifies multiple specifications"
        ))),
        Some(_) => Ok(()),
        None => {
            seen.insert(name.clone(), spec.clone());
            Ok(())
        }
    }
}

fn same_spec(
    context: &EvaluatorStepContext<'_>,
    left: &RuntimeValueRoot,
    right: &RuntimeValueRoot,
) -> bool {
    context.with_value_access(|access| {
        let (Value::Dict(left), Value::Dict(right)) =
            (access.clone_root(left), access.clone_root(right))
        else {
            unreachable!("linearized object specs must retain dictionaries")
        };
        left.ptr_eq(&right)
    })
}

fn is_anonymous_object_name(name: &Key) -> bool {
    matches!(name, Key::Dict(entries) if entries.is_empty())
}

fn c3_merge(
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
            .then(|| candidate.clone())
        });
        let Some(selected) = selected else {
            return Err(EvaluationHalt::new(
                "object dependencies have inconsistent C3 linearization",
            ));
        };
        result.push(selected.clone());
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

fn finish_object(
    context: &EvaluatorStepContext<'_>,
    base: RuntimeValueRoot,
    spec: &RuntimeValueRoot,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    context.with_value_access(|access| {
        let Value::Dict(base) = access.clone_root(&base) else {
            return Err(EvaluationHalt::new("object base is not a dictionary"));
        };
        let spec = access.clone_root(spec);
        Ok(access
            .values()
            .root_runtime_value(Value::Dict(base.insert((*keys::SPEC).clone(), spec))))
    })
}

fn root_halt(context: &EvaluatorStepContext<'_>, halt: EvaluationHalt) -> RuntimeFailureRoot {
    context.root_failure(halt.into_permanent_failure())
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
