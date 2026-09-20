//! Pollable logical-list projection shared by source-owned computations.
//!
//! The persistent list representation itself performs only non-forcing
//! decomposition. This owner retains the exact deferred chunk and logical
//! suffix while ordinary WHNF orchestration resolves that chunk.

use std::sync::{Arc, Mutex, TryLockError};

use glam_gc::{Root, Trace, Visitor};

use crate::core::{
    EvaluationFailure, EvaluationHalt, LazyId, List, ManagedDropRecord, ManagedFamily, Value,
    managed_slot_extent, trace_compatibility_value_managed_edges,
};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluationValueAccess, EvaluatorStepContext, WhnfOwnerPoll,
    interpret_whnf_poll,
};
use crate::list::{ListBackStep, ListFrontStep, ListItem};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, WhnfPoll,
    drive_regional_in_place, reduce_semantic_shell,
};

pub(crate) enum ListFrontPoll {
    Ready(Option<(RuntimeValueRoot, RuntimeValueRoot)>),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) enum ListBackPoll {
    Ready(Option<(RuntimeValueRoot, RuntimeValueRoot)>),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) struct ListFrontMachine {
    checkpoint: DurableListFrontCheckpoint,
}

enum DurableListFrontCheckpoint {
    Seed {
        list: RuntimeValueRoot,
        source_owner: Option<LazyId>,
    },
    Managed(ManagedListFrontRoot),
}

struct ManagedListFrontRoot {
    root: Root<ManagedListFrontCell>,
}

struct ManagedListFrontCell {
    state: Mutex<RegionalListFront>,
}

/// Callback-free logical-list projection beneath one value-access region.
///
/// Every semantic edge is raw and reported by [`Self::trace_managed_edges`],
/// so this state may be nested directly inside another managed checkpoint.
pub(in crate::eval) struct RegionalListFront {
    current: Value,
    chunk: Option<RegionalWhnfWork>,
    suffix: Option<Value>,
    source_owner: Option<LazyId>,
}

pub(in crate::eval) enum RegionalListFrontPoll {
    Ready(Option<(Value, Value)>),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

/// Callback-free logical-list back projection beneath one value-access
/// region.
///
/// This is the back-oriented counterpart to [`RegionalListFront`]. Every
/// semantic edge is raw and reported by [`Self::trace_managed_edges`], so a
/// builtin checkpoint may retain it without embedding registered roots.
pub(in crate::eval) struct RegionalListBack {
    current: Value,
    chunk: Option<RegionalWhnfWork>,
    prefix: Option<Value>,
    source_owner: Option<LazyId>,
}

pub(in crate::eval) enum RegionalListBackPoll {
    Ready(Option<(Value, Value)>),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

enum DurableListFrontPoll {
    Ready(Option<(RuntimeValueRoot, RuntimeValueRoot)>),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) struct ListBackMachine {
    checkpoint: DurableListBackCheckpoint,
}

enum DurableListBackCheckpoint {
    Seed {
        list: RuntimeValueRoot,
        source_owner: Option<LazyId>,
    },
    Managed(ManagedListBackRoot),
}

struct ManagedListBackRoot {
    root: Root<ManagedListBackCell>,
}

struct ManagedListBackCell {
    state: Mutex<RegionalListBack>,
}

enum DurableListBackPoll {
    Ready(Option<(RuntimeValueRoot, RuntimeValueRoot)>),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl ListFrontMachine {
    pub(crate) fn unowned(list: RuntimeValueRoot) -> Self {
        Self {
            checkpoint: DurableListFrontCheckpoint::Seed {
                list,
                source_owner: None,
            },
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        _context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ListFrontPoll {
        let result = poll_context.with_value_access(durable_context, |access| {
            self.promote_in(&access);
            let DurableListFrontCheckpoint::Managed(root) = &self.checkpoint else {
                unreachable!("list-front seed must promote under access")
            };
            root.poll_in(&access, step_budget)
        });
        interpret_durable_list_front(result, durable_context)
    }

    fn promote_in(&mut self, access: &EvaluationValueAccess<'_>) {
        let DurableListFrontCheckpoint::Seed { list, source_owner } = &self.checkpoint else {
            return;
        };
        let state = RegionalListFront::new_in(access, access.clone_root(list), *source_owner);
        self.checkpoint =
            DurableListFrontCheckpoint::Managed(ManagedListFrontRoot::new_in(access, state));
    }
}

impl RegionalListFront {
    pub(in crate::eval) fn new_in(
        _access: &EvaluationValueAccess<'_>,
        list: Value,
        source_owner: Option<LazyId>,
    ) -> Self {
        debug_assert!(matches!(list, Value::List(_)));
        Self {
            current: list,
            chunk: None,
            suffix: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalListFrontPoll {
        if let Some(chunk) = &mut self.chunk {
            let value =
                match drive_regional_in_place(access, chunk, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalListFrontPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalListFrontPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalListFrontPoll::Failed(failure);
                    }
                };
            let suffix = self
                .suffix
                .take()
                .expect("deferred list work must retain its exact suffix");
            self.current = match combine_regional_chunk_and_suffix(access, value, suffix) {
                Ok(list) => list,
                Err(error) => return RegionalListFrontPoll::Failed(error),
            };
            self.chunk = None;
            return RegionalListFrontPoll::Yielded;
        }

        let Value::List(list) = access.values().duplicate_value(&self.current) else {
            unreachable!("logical-list-front work must retain a list value")
        };
        match list.pop_front_step_by(
            &mut |value| access.values().duplicate_value(value),
            &mut |thunk| thunk.duplicate_as_value_in(access.values()),
        ) {
            ListFrontStep::Empty => RegionalListFrontPoll::Ready(None),
            ListFrontStep::Item { item, tail } => {
                let value = match item {
                    ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
                    ListItem::Value(value) => value,
                };
                RegionalListFrontPoll::Ready(Some((value, Value::List(tail))))
            }
            ListFrontStep::Deferred { deferred, suffix } => {
                let mut chunk = RegionalWhnfWork::from_focus(access, deferred);
                if let Some(source_owner) = self.source_owner {
                    chunk = chunk.with_source_owner(source_owner);
                }
                self.chunk = Some(chunk);
                self.suffix = Some(Value::List(suffix));
                RegionalListFrontPoll::Yielded
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.current, visitor);
        if let Some(chunk) = &self.chunk {
            chunk.trace_managed_edges(visitor);
        }
        if let Some(suffix) = &self.suffix {
            trace_compatibility_value_managed_edges(suffix, visitor);
        }
    }
}

impl RegionalListBack {
    pub(in crate::eval) fn new_in(
        _access: &EvaluationValueAccess<'_>,
        list: Value,
        source_owner: Option<LazyId>,
    ) -> Self {
        debug_assert!(matches!(list, Value::List(_)));
        Self {
            current: list,
            chunk: None,
            prefix: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalListBackPoll {
        if let Some(chunk) = &mut self.chunk {
            let value =
                match drive_regional_in_place(access, chunk, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalListBackPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalListBackPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalListBackPoll::Failed(failure);
                    }
                };
            let prefix = self
                .prefix
                .take()
                .expect("deferred back-list work must retain its exact prefix");
            self.current = match combine_regional_prefix_and_chunk(access, prefix, value) {
                Ok(list) => list,
                Err(error) => return RegionalListBackPoll::Failed(error),
            };
            self.chunk = None;
            return RegionalListBackPoll::Yielded;
        }

        let Value::List(list) = access.values().duplicate_value(&self.current) else {
            unreachable!("logical-list-back work must retain a list value")
        };
        match list.pop_back_step_by(
            &mut |value| access.values().duplicate_value(value),
            &mut |thunk| thunk.duplicate_as_value_in(access.values()),
        ) {
            ListBackStep::Empty => RegionalListBackPoll::Ready(None),
            ListBackStep::Item { init, item } => {
                let value = match item {
                    ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
                    ListItem::Value(value) => value,
                };
                RegionalListBackPoll::Ready(Some((Value::List(init), value)))
            }
            ListBackStep::Deferred { prefix, deferred } => {
                let mut chunk = RegionalWhnfWork::from_focus(access, deferred);
                if let Some(source_owner) = self.source_owner {
                    chunk = chunk.with_source_owner(source_owner);
                }
                self.chunk = Some(chunk);
                self.prefix = Some(Value::List(prefix));
                RegionalListBackPoll::Yielded
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.current, visitor);
        if let Some(chunk) = &self.chunk {
            chunk.trace_managed_edges(visitor);
        }
        if let Some(prefix) = &self.prefix {
            trace_compatibility_value_managed_edges(prefix, visitor);
        }
    }
}

impl ManagedListFrontRoot {
    fn new_in(access: &EvaluationValueAccess<'_>, state: RegionalListFront) -> Self {
        let edge = access
            .values()
            .allocator::<ManagedListFrontCell>()
            .expect("managed list-front representation must fit one collector run")
            .alloc(ManagedListFrontCell {
                state: Mutex::new(state),
            });
        Self {
            root: access.values().root(edge),
        }
    }

    fn poll_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> DurableListFrontPoll {
        assert!(
            access.values().admits_root(&self.root),
            "list-front checkpoint must share the evaluator value domain"
        );
        let owner = access.values().project_root(&self.root);
        let cell = access.values().get(&self.root);
        let mut state = match cell.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return DurableListFrontPoll::Failed(access.values().root_runtime_failure(
                    Arc::new(EvaluationFailure::message(
                        "managed list-front state was poisoned by an earlier unwind",
                    )),
                ));
            }
        };
        // SAFETY: the registered wrapper root keeps `owner` live in this
        // exact access region. The cell mutex excludes another transition,
        // and the same compile-exhaustive visitor reports every raw edge
        // before and after mutation.
        let result = unsafe {
            access.values().with_managed_edge_state_transition(
                &owner,
                &mut *state,
                RegionalListFront::trace_managed_edges,
                RegionalListFront::trace_managed_edges,
                |state| state.poll_in(access, step_budget),
            )
        };
        match result {
            RegionalListFrontPoll::Ready(Some((item, tail))) => {
                DurableListFrontPoll::Ready(Some((
                    access.values().root_runtime_value(item),
                    access.values().root_runtime_value(tail),
                )))
            }
            RegionalListFrontPoll::Ready(None) => DurableListFrontPoll::Ready(None),
            RegionalListFrontPoll::Boundary(request) => DurableListFrontPoll::Boundary(request),
            RegionalListFrontPoll::Yielded => DurableListFrontPoll::Yielded,
            RegionalListFrontPoll::Failed(failure) => {
                DurableListFrontPoll::Failed(access.values().root_runtime_failure(failure))
            }
        }
    }
}

fn interpret_durable_list_front(
    result: DurableListFrontPoll,
    context: &EvalContext,
) -> ListFrontPoll {
    match result {
        DurableListFrontPoll::Ready(value) => ListFrontPoll::Ready(value),
        DurableListFrontPoll::Boundary(request) => {
            let poll = match request {
                RegionalBoundaryRequest::Dependency(dependency) => WhnfPoll::Pending(dependency),
                RegionalBoundaryRequest::Deferred(deferred) => WhnfPoll::Deferred(deferred),
                RegionalBoundaryRequest::External(boundary) => WhnfPoll::External(boundary),
            };
            match interpret_whnf_poll(poll, context) {
                WhnfOwnerPoll::Pending(dependency) => ListFrontPoll::Pending(dependency),
                WhnfOwnerPoll::Yielded => ListFrontPoll::Yielded,
                WhnfOwnerPoll::Failed(failure) => ListFrontPoll::Failed(failure),
                WhnfOwnerPoll::External(boundary) => {
                    unreachable!("lazy list chunk produced an external {boundary:?} boundary")
                }
                WhnfOwnerPoll::Ready(_) => {
                    unreachable!("a semantic boundary cannot produce an immediate value")
                }
            }
        }
        DurableListFrontPoll::Yielded => ListFrontPoll::Yielded,
        DurableListFrontPoll::Failed(failure) => ListFrontPoll::Failed(failure),
    }
}

impl ListBackMachine {
    pub(crate) fn new(list: RuntimeValueRoot) -> Self {
        Self {
            checkpoint: DurableListBackCheckpoint::Seed {
                list,
                source_owner: None,
            },
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        _context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ListBackPoll {
        let result = poll_context.with_value_access(durable_context, |access| {
            self.promote_in(&access);
            let DurableListBackCheckpoint::Managed(root) = &self.checkpoint else {
                unreachable!("list-back seed must promote under access")
            };
            root.poll_in(&access, step_budget)
        });
        interpret_durable_list_back(result, durable_context)
    }

    fn promote_in(&mut self, access: &EvaluationValueAccess<'_>) {
        let DurableListBackCheckpoint::Seed { list, source_owner } = &self.checkpoint else {
            return;
        };
        let state = RegionalListBack::new_in(access, access.clone_root(list), *source_owner);
        self.checkpoint =
            DurableListBackCheckpoint::Managed(ManagedListBackRoot::new_in(access, state));
    }
}

impl ManagedListBackRoot {
    fn new_in(access: &EvaluationValueAccess<'_>, state: RegionalListBack) -> Self {
        let edge = access
            .values()
            .allocator::<ManagedListBackCell>()
            .expect("managed list-back representation must fit one collector run")
            .alloc(ManagedListBackCell {
                state: Mutex::new(state),
            });
        Self {
            root: access.values().root(edge),
        }
    }

    fn poll_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> DurableListBackPoll {
        assert!(
            access.values().admits_root(&self.root),
            "list-back checkpoint must share the evaluator value domain"
        );
        let owner = access.values().project_root(&self.root);
        let cell = access.values().get(&self.root);
        let mut state = match cell.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return DurableListBackPoll::Failed(access.values().root_runtime_failure(
                    Arc::new(EvaluationFailure::message(
                        "managed list-back state was poisoned by an earlier unwind",
                    )),
                ));
            }
        };
        // SAFETY: the registered wrapper root keeps `owner` live in this
        // exact access region. The cell mutex excludes another transition,
        // and the same compile-exhaustive visitor reports every raw edge
        // before and after mutation.
        let result = unsafe {
            access.values().with_managed_edge_state_transition(
                &owner,
                &mut *state,
                RegionalListBack::trace_managed_edges,
                RegionalListBack::trace_managed_edges,
                |state| state.poll_in(access, step_budget),
            )
        };
        match result {
            RegionalListBackPoll::Ready(Some((init, item))) => DurableListBackPoll::Ready(Some((
                access.values().root_runtime_value(init),
                access.values().root_runtime_value(item),
            ))),
            RegionalListBackPoll::Ready(None) => DurableListBackPoll::Ready(None),
            RegionalListBackPoll::Boundary(request) => DurableListBackPoll::Boundary(request),
            RegionalListBackPoll::Yielded => DurableListBackPoll::Yielded,
            RegionalListBackPoll::Failed(failure) => {
                DurableListBackPoll::Failed(access.values().root_runtime_failure(failure))
            }
        }
    }
}

fn interpret_durable_list_back(result: DurableListBackPoll, context: &EvalContext) -> ListBackPoll {
    match result {
        DurableListBackPoll::Ready(value) => ListBackPoll::Ready(value),
        DurableListBackPoll::Boundary(request) => {
            let poll = match request {
                RegionalBoundaryRequest::Dependency(dependency) => WhnfPoll::Pending(dependency),
                RegionalBoundaryRequest::Deferred(deferred) => WhnfPoll::Deferred(deferred),
                RegionalBoundaryRequest::External(boundary) => WhnfPoll::External(boundary),
            };
            match interpret_whnf_poll(poll, context) {
                WhnfOwnerPoll::Pending(dependency) => ListBackPoll::Pending(dependency),
                WhnfOwnerPoll::Yielded => ListBackPoll::Yielded,
                WhnfOwnerPoll::Failed(failure) => ListBackPoll::Failed(failure),
                WhnfOwnerPoll::External(boundary) => {
                    unreachable!("lazy list chunk produced an external {boundary:?} boundary")
                }
                WhnfOwnerPoll::Ready(_) => {
                    unreachable!("a semantic boundary cannot produce an immediate value")
                }
            }
        }
        DurableListBackPoll::Yielded => ListBackPoll::Yielded,
        DurableListBackPoll::Failed(failure) => ListBackPoll::Failed(failure),
    }
}

fn combine_regional_chunk_and_suffix(
    _access: &EvaluationValueAccess<'_>,
    chunk: Value,
    suffix: Value,
) -> Result<Value, Arc<EvaluationFailure>> {
    let chunk = match chunk {
        Value::Binary(bytes) => List::from_bytes(bytes),
        Value::List(list) => list,
        other => {
            return Err(EvaluationHalt::new(format!(
                "lazy list chunk must evaluate to a list or binary value, got {other:?}"
            ))
            .into_permanent_failure());
        }
    };
    let Value::List(suffix) = suffix else {
        unreachable!("logical-list-front work must retain a list suffix")
    };
    Ok(Value::List(List::concat(chunk, suffix)))
}

fn combine_regional_prefix_and_chunk(
    _access: &EvaluationValueAccess<'_>,
    prefix: Value,
    chunk: Value,
) -> Result<Value, Arc<EvaluationFailure>> {
    let Value::List(prefix) = prefix else {
        unreachable!("logical-list-back work must retain a list prefix")
    };
    let chunk = match chunk {
        Value::Binary(bytes) => List::from_bytes(bytes),
        Value::List(list) => list,
        other => {
            return Err(EvaluationHalt::new(format!(
                "lazy list chunk must evaluate to a list or binary value, got {other:?}"
            ))
            .into_permanent_failure());
        }
    };
    Ok(Value::List(List::concat(prefix, chunk)))
}

// SAFETY: the state visitor is compile-exhaustive over the current logical
// list, deferred WHNF child, and exact suffix. Collection runs only after
// mutator quiescence, so an unpoisoned busy mutex is an invariant failure.
unsafe impl Trace for ManagedListFrontCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("managed list-front state must be quiescent during tracing")
            }
        };
        state.trace_managed_edges(visitor);
    }
}

// SAFETY: direct destruction releases only passive compatibility values,
// regional WHNF state, and scalar identities. It invokes no runtime,
// evaluator, scheduler, host, or diagnostic capability.
unsafe impl ManagedFamily for ManagedListFrontCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "temporary managed list-front checkpoint",
        "src/eval/list_machine.rs",
        "no direct Drop implementation",
        "regional list projection and WHNF state destroy passively",
    );
}

// SAFETY: the state visitor is compile-exhaustive over the current logical
// list, deferred WHNF child, and exact prefix. Collection runs only after
// mutator quiescence, so an unpoisoned busy mutex is an invariant failure.
unsafe impl Trace for ManagedListBackCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("managed list-back state must be quiescent during tracing")
            }
        };
        state.trace_managed_edges(visitor);
    }
}

// SAFETY: direct destruction releases only passive compatibility values,
// regional WHNF state, and scalar identities. It invokes no runtime,
// evaluator, scheduler, host, or diagnostic capability.
unsafe impl ManagedFamily for ManagedListBackCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "temporary managed list-back checkpoint",
        "src/eval/list_machine.rs",
        "no direct Drop implementation",
        "regional list projection and WHNF state destroy passively",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CoreValueFactory, ListThunk, PromisedValue};
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    fn poll_front(
        machine: &mut ListFrontMachine,
        context: &EvalContext,
        allowance: usize,
    ) -> ListFrontPoll {
        let poll = EvaluationPollContext::for_context(context);
        poll.evaluate(context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                context,
                &mut crate::evaluation::EvaluationStepBudget::new(allowance),
            )
        })
    }

    fn poll_back(
        machine: &mut ListBackMachine,
        context: &EvalContext,
        allowance: usize,
    ) -> ListBackPoll {
        let poll = EvaluationPollContext::for_context(context);
        poll.evaluate(context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                context,
                &mut crate::evaluation::EvaluationStepBudget::new(allowance),
            )
        })
    }

    #[test]
    fn front_projection_uses_one_managed_root_and_survives_deferred_collection() {
        let context = context();
        let chunk = PromisedValue::new(context.values(), "regional list-front chunk");
        let list = context.values().construct_runtime_value_root(|_| {
            Value::List(List::concat(
                List::from_thunk(ListThunk::Promised(chunk.clone())),
                List::from_values(vec![Value::Number(4.into())]),
            ))
        });
        let registrations = context.values().managed_root_registrations_for_test();
        let mut machine = ListFrontMachine::unowned(list);

        assert!(matches!(
            poll_front(&mut machine, &context, 1),
            ListFrontPoll::Yielded
        ));
        assert_eq!(
            context.values().managed_root_registrations_for_test(),
            registrations + 1,
            "promotion must install one managed list-front owner"
        );
        context
            .values()
            .collect_managed_for_test()
            .expect("the deferred chunk and suffix must remain traced after promotion");

        assert!(matches!(
            poll_front(&mut machine, &context, 1),
            ListFrontPoll::Pending(_)
        ));
        context
            .values()
            .collect_managed_for_test()
            .expect("the exact promise wait must remain traced across collection");
        crate::core::set_test_promise(
            context.values(),
            &chunk,
            Value::Binary(bytes::Bytes::from_static(&[2_u8, 3_u8])),
        )
        .expect("the deferred binary chunk should accept its assignment");

        let (item, tail) = loop {
            match poll_front(&mut machine, &context, 1) {
                ListFrontPoll::Ready(Some(result)) => break result,
                ListFrontPoll::Yielded => {
                    context
                        .values()
                        .collect_managed_for_test()
                        .expect("every yielded regional transition must retain its edges");
                }
                ListFrontPoll::Pending(_) => {
                    panic!("an assigned chunk must not return to a promise wait")
                }
                ListFrontPoll::Ready(None) => panic!("the resumed list must not be empty"),
                ListFrontPoll::Failed(failure) => {
                    panic!("the deferred binary chunk failed: {failure:?}")
                }
            }
        };

        assert_eq!(item.clone_core_for_test(), Value::Number(2.into()));
        assert_eq!(
            tail.clone_core_for_test(),
            Value::List(List::concat(
                List::from_bytes(bytes::Bytes::from_static(&[3_u8])),
                List::from_values(vec![Value::Number(4.into())]),
            ))
        );
    }

    #[test]
    fn back_projection_uses_one_managed_root_and_survives_deferred_collection() {
        let context = context();
        let chunk = PromisedValue::new(context.values(), "regional list-back chunk");
        let list = context.values().construct_runtime_value_root(|_| {
            Value::List(List::concat(
                List::from_values(vec![Value::Number(1.into())]),
                List::from_thunk(ListThunk::Promised(chunk.clone())),
            ))
        });
        let registrations = context.values().managed_root_registrations_for_test();
        let mut machine = ListBackMachine::new(list);

        assert!(matches!(
            poll_back(&mut machine, &context, 1),
            ListBackPoll::Yielded
        ));
        assert_eq!(
            context.values().managed_root_registrations_for_test(),
            registrations + 1,
            "promotion must install one managed list-back owner"
        );
        context
            .values()
            .collect_managed_for_test()
            .expect("the deferred chunk and prefix must remain traced after promotion");

        assert!(matches!(
            poll_back(&mut machine, &context, 1),
            ListBackPoll::Pending(_)
        ));
        context
            .values()
            .collect_managed_for_test()
            .expect("the exact promise wait must remain traced across collection");
        crate::core::set_test_promise(
            context.values(),
            &chunk,
            Value::Binary(bytes::Bytes::from_static(&[2_u8, 3_u8])),
        )
        .expect("the deferred binary chunk should accept its assignment");

        let (init, item) = loop {
            match poll_back(&mut machine, &context, 1) {
                ListBackPoll::Ready(Some(result)) => break result,
                ListBackPoll::Yielded => {
                    context
                        .values()
                        .collect_managed_for_test()
                        .expect("every yielded regional transition must retain its edges");
                }
                ListBackPoll::Pending(_) => {
                    panic!("an assigned chunk must not return to a promise wait")
                }
                ListBackPoll::Ready(None) => panic!("the resumed list must not be empty"),
                ListBackPoll::Failed(failure) => {
                    panic!("the deferred binary chunk failed: {failure:?}")
                }
            }
        };

        assert_eq!(item.clone_core_for_test(), Value::Number(3.into()));
        assert_eq!(
            init.clone_core_for_test(),
            Value::List(List::concat(
                List::from_values(vec![Value::Number(1.into())]),
                List::from_bytes(bytes::Bytes::from_static(&[2_u8])),
            ))
        );
    }
}
