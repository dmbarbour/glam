//! Resumable computed dictionary access and recursive key conversion.
//!
//! This is source-owned semantic state, not a second evaluator. Every value
//! which crosses a poll boundary is either held by a traced managed checkpoint
//! or by the temporary durable wrapper used by unmigrated parents. Ordinary
//! WHNF demand and dependency admission remain delegated to the canonical
//! regional reducer and its scheduler adapter.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};

use glam_gc::{Root, Trace, Visitor};

use crate::core::{
    Dict, EvaluationFailure, EvaluationHalt, Key, LazyId, List, ManagedDropRecord, ManagedFamily,
    Value, managed_slot_extent, trace_compatibility_value_managed_edges,
};
use crate::core_net::CoreDataKey;
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluationValueAccess, EvaluatorStepContext, WhnfOwnerPoll,
    interpret_whnf_poll,
};
use crate::list::{ListFrontStep, ListItem};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, WhnfPoll,
    drive_regional_in_place, reduce_semantic_shell,
};

pub(in crate::eval) enum AccessRegionalPoll {
    Ready(Value),
    Whnf(RegionalWhnfWork),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

pub(in crate::eval) struct AccessMachine {
    path: Arc<[CoreDataKey]>,
    arguments: Vec<Value>,
    next_part: usize,
    next_argument: usize,
    pending_keys: VecDeque<Key>,
    current: Value,
    demand: Option<RegionalWhnfWork>,
    conversion: Option<AccessConversion>,
    source_owner: LazyId,
}

enum AccessConversion {
    Key(RegionalKeyConversion),
    Path(Box<RegionalKeyList>),
}

pub(crate) enum ConversionPoll<T> {
    Ready(T),
    Pending(crate::evaluation::WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) struct KeyConversionMachine {
    checkpoint: DurableKeyConversionCheckpoint,
}

pub(crate) struct KeyListMachine {
    checkpoint: DurableKeyListCheckpoint,
}

enum DurableKeyConversionCheckpoint {
    Seed {
        value: RuntimeValueRoot,
        source_owner: Option<LazyId>,
    },
    Managed(ManagedKeyConversionRoot),
}

enum DurableKeyListCheckpoint {
    Seed {
        value: RuntimeValueRoot,
        ready: bool,
        source_owner: Option<LazyId>,
    },
    Managed(ManagedKeyConversionRoot),
}

struct ManagedKeyConversionRoot {
    root: Root<ManagedKeyConversionCell>,
}

struct ManagedKeyConversionCell {
    state: Mutex<ManagedKeyConversionState>,
}

enum ManagedKeyConversionState {
    Key(RegionalKeyConversion),
    List(Box<RegionalKeyList>),
}

pub(in crate::eval) struct RegionalKeyConversion {
    state: RegionalKeyConversionState,
    source_owner: Option<LazyId>,
}

enum RegionalKeyConversionState {
    Demand(RegionalWhnfWork),
    Dict(RegionalDictConversion),
    List(Box<RegionalKeyList>),
}

enum RegionalClassifiedKeyValue {
    Ready(Key),
    Dict(Vec<(Key, Value)>),
    List(Value),
    Invalid,
}

struct RegionalDictConversion {
    members: Vec<(Key, Value)>,
    next: usize,
    converted: Vec<(Key, Key)>,
    child: Option<Box<RegionalKeyConversion>>,
}

/// Callback-free conversion of one logical list into dictionary keys.
///
/// The list spine, deferred chunks, recursive item conversion, and completed
/// prefix remain raw managed edges beneath one caller-owned checkpoint. This
/// is also the shared child representation for compiler paths.
pub(in crate::eval) struct RegionalKeyList {
    source: Option<RegionalWhnfWork>,
    lists: Vec<Value>,
    chunk: Option<RegionalWhnfWork>,
    chunk_suffix: Option<Value>,
    child: Option<Box<RegionalKeyConversion>>,
    converted: Vec<Key>,
    source_owner: Option<LazyId>,
}

pub(in crate::eval) enum RegionalConversionPoll<T> {
    Ready(T),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

enum DurableConversionPoll<T> {
    Ready(T),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl AccessMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        path: Arc<[CoreDataKey]>,
        arguments: &[Value],
    ) -> Self {
        let current = arguments
            .first()
            .map(|value| access.values().duplicate_value(value))
            .expect("value access must retain its base value");
        Self {
            path,
            arguments: arguments
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect(),
            next_part: 0,
            next_argument: 1,
            pending_keys: VecDeque::new(),
            current,
            demand: None,
            conversion: None,
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> AccessRegionalPoll {
        if let Some(conversion) = &mut self.conversion {
            let result = match conversion {
                AccessConversion::Key(machine) => machine.poll_optional_in(access, step_budget),
                AccessConversion::Path(machine) => {
                    return match machine.poll_optional_in(access, step_budget) {
                        RegionalConversionPoll::Ready(Some(keys)) => {
                            self.conversion = None;
                            self.next_part += 1;
                            self.begin_key_sequence(access, keys);
                            AccessRegionalPoll::Yielded
                        }
                        RegionalConversionPoll::Ready(None) => {
                            AccessRegionalPoll::Failed(Arc::new(EvaluationFailure::message(
                                "dictionary keys must evaluate to keyable values",
                            )))
                        }
                        RegionalConversionPoll::Boundary(request) => {
                            AccessRegionalPoll::Boundary(request)
                        }
                        RegionalConversionPoll::Yielded => AccessRegionalPoll::Yielded,
                        RegionalConversionPoll::Failed(failure) => {
                            AccessRegionalPoll::Failed(failure)
                        }
                    };
                }
            };
            return match result {
                RegionalConversionPoll::Ready(Some(key)) => {
                    self.conversion = None;
                    self.next_part += 1;
                    self.begin_key_sequence(access, [key]);
                    AccessRegionalPoll::Yielded
                }
                RegionalConversionPoll::Ready(None) => AccessRegionalPoll::Failed(Arc::new(
                    EvaluationFailure::message("dictionary keys must evaluate to keyable values"),
                )),
                RegionalConversionPoll::Boundary(request) => AccessRegionalPoll::Boundary(request),
                RegionalConversionPoll::Yielded => AccessRegionalPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => AccessRegionalPoll::Failed(failure),
            };
        }

        if let Some(demand) = &mut self.demand {
            let current = match poll_regional_whnf(demand, access, step_budget) {
                RegionalConversionPoll::Ready(value) => value,
                RegionalConversionPoll::Boundary(request) => {
                    return AccessRegionalPoll::Boundary(request);
                }
                RegionalConversionPoll::Yielded => return AccessRegionalPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => {
                    return AccessRegionalPoll::Failed(failure);
                }
            };
            self.demand = None;
            let Some(key) = self.pending_keys.pop_front() else {
                debug_assert_eq!(self.next_part, self.path.len());
                return AccessRegionalPoll::Ready(current);
            };
            return match select_dict_member_in(access, &current, &key) {
                Ok(value) => {
                    self.current = value;
                    if !self.pending_keys.is_empty() {
                        self.demand = Some(regional_whnf(
                            access,
                            access.values().duplicate_value(&self.current),
                            Some(self.source_owner),
                        ));
                    }
                    AccessRegionalPoll::Yielded
                }
                Err(error) => AccessRegionalPoll::Failed(error.into_permanent_failure()),
            };
        }

        let Some(part) = self.path.get(self.next_part) else {
            return AccessRegionalPoll::Whnf(regional_whnf(
                access,
                access.values().duplicate_value(&self.current),
                Some(self.source_owner),
            ));
        };
        match part {
            CoreDataKey::Key(key) => {
                self.next_part += 1;
                self.begin_key_sequence(access, [key.clone()]);
            }
            CoreDataKey::Index => {
                let argument = self.next_dynamic_argument(access);
                self.conversion = Some(AccessConversion::Key(RegionalKeyConversion::new(
                    access,
                    argument,
                    Some(self.source_owner),
                )));
            }
            CoreDataKey::PathIndex => {
                let argument = self.next_dynamic_argument(access);
                self.conversion = Some(AccessConversion::Path(Box::new(RegionalKeyList::new(
                    access,
                    argument,
                    Some(self.source_owner),
                ))));
            }
        }
        AccessRegionalPoll::Yielded
    }

    fn next_dynamic_argument(&mut self, access: &EvaluationValueAccess<'_>) -> Value {
        let argument = self
            .arguments
            .get(self.next_argument)
            .map(|value| access.values().duplicate_value(value))
            .expect("lowered access index must retain its argument");
        self.next_argument += 1;
        argument
    }

    fn begin_key_sequence(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        keys: impl IntoIterator<Item = Key>,
    ) {
        self.pending_keys.extend(keys);
        if !self.pending_keys.is_empty() {
            self.demand = Some(regional_whnf(
                access,
                access.values().duplicate_value(&self.current),
                Some(self.source_owner),
            ));
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for argument in &self.arguments {
            trace_compatibility_value_managed_edges(argument, visitor);
        }
        trace_compatibility_value_managed_edges(&self.current, visitor);
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(conversion) = &self.conversion {
            match conversion {
                AccessConversion::Key(machine) => machine.trace_managed_edges(visitor),
                AccessConversion::Path(machine) => machine.trace_managed_edges(visitor),
            }
        }
    }
}

impl KeyConversionMachine {
    pub(crate) fn new(value: RuntimeValueRoot, source_owner: Option<LazyId>) -> Self {
        Self {
            checkpoint: DurableKeyConversionCheckpoint::Seed {
                value,
                source_owner,
            },
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ConversionPoll<Key> {
        match self.poll_optional(poll_context, context, durable_context, step_budget) {
            ConversionPoll::Ready(Some(key)) => ConversionPoll::Ready(key),
            ConversionPoll::Ready(None) => ConversionPoll::Failed(root_message(
                context,
                "dictionary keys must evaluate to keyable values",
            )),
            ConversionPoll::Pending(dependency) => ConversionPoll::Pending(dependency),
            ConversionPoll::Yielded => ConversionPoll::Yielded,
            ConversionPoll::Failed(failure) => ConversionPoll::Failed(failure),
        }
    }

    pub(crate) fn poll_optional(
        &mut self,
        poll_context: &EvaluationPollContext,
        _context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ConversionPoll<Option<Key>> {
        let result = poll_context.with_value_access(durable_context, |access| {
            self.promote_in(&access);
            let DurableKeyConversionCheckpoint::Managed(root) = &self.checkpoint else {
                unreachable!("key conversion seed must promote under access")
            };
            root.poll_key_in(&access, step_budget)
        });
        interpret_durable_conversion(result, durable_context)
    }

    fn promote_in(&mut self, access: &EvaluationValueAccess<'_>) {
        let DurableKeyConversionCheckpoint::Seed {
            value,
            source_owner,
        } = &self.checkpoint
        else {
            return;
        };
        let state = ManagedKeyConversionState::Key(RegionalKeyConversion::new(
            access,
            access.clone_root(value),
            *source_owner,
        ));
        self.checkpoint = DurableKeyConversionCheckpoint::Managed(
            ManagedKeyConversionRoot::new_in(access, state),
        );
    }
}

impl KeyListMachine {
    fn new(value: RuntimeValueRoot, source_owner: Option<LazyId>) -> Self {
        Self {
            checkpoint: DurableKeyListCheckpoint::Seed {
                value,
                ready: false,
                source_owner,
            },
        }
    }

    fn from_ready(value: RuntimeValueRoot, source_owner: Option<LazyId>) -> Self {
        Self {
            checkpoint: DurableKeyListCheckpoint::Seed {
                value,
                ready: true,
                source_owner,
            },
        }
    }

    pub(crate) fn unowned(value: RuntimeValueRoot) -> Self {
        Self::new(value, None)
    }

    pub(crate) fn from_ready_unowned(value: RuntimeValueRoot) -> Self {
        Self::from_ready(value, None)
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        _context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ConversionPoll<Vec<Key>> {
        let result = poll_context.with_value_access(durable_context, |access| {
            self.promote_in(&access);
            let DurableKeyListCheckpoint::Managed(root) = &self.checkpoint else {
                unreachable!("key-list seed must promote under access")
            };
            root.poll_list_in(&access, step_budget)
        });
        interpret_durable_conversion(result, durable_context)
    }

    pub(crate) fn poll_optional(
        &mut self,
        poll_context: &EvaluationPollContext,
        _context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> ConversionPoll<Option<Vec<Key>>> {
        let result = poll_context.with_value_access(durable_context, |access| {
            self.promote_in(&access);
            let DurableKeyListCheckpoint::Managed(root) = &self.checkpoint else {
                unreachable!("key-list seed must promote under access")
            };
            root.poll_list_optional_in(&access, step_budget)
        });
        interpret_durable_conversion(result, durable_context)
    }

    fn promote_in(&mut self, access: &EvaluationValueAccess<'_>) {
        let DurableKeyListCheckpoint::Seed {
            value,
            ready,
            source_owner,
        } = &self.checkpoint
        else {
            return;
        };
        let value = access.clone_root(value);
        let state = if *ready {
            RegionalKeyList::from_ready(access, value, *source_owner)
        } else {
            RegionalKeyList::new(access, value, *source_owner)
        };
        self.checkpoint = DurableKeyListCheckpoint::Managed(ManagedKeyConversionRoot::new_in(
            access,
            ManagedKeyConversionState::List(Box::new(state)),
        ));
    }
}

impl ManagedKeyConversionRoot {
    fn new_in(access: &EvaluationValueAccess<'_>, state: ManagedKeyConversionState) -> Self {
        let edge = access
            .values()
            .allocator::<ManagedKeyConversionCell>()
            .expect("managed key conversion representation must fit one collector run")
            .alloc(ManagedKeyConversionCell {
                state: Mutex::new(state),
            });
        Self {
            root: access.values().root(edge),
        }
    }

    fn poll_key_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> DurableConversionPoll<Option<Key>> {
        self.with_state_transition(access, |state| {
            let ManagedKeyConversionState::Key(state) = state else {
                panic!("key conversion wrapper retained list conversion state")
            };
            state.poll_optional_in(access, step_budget)
        })
    }

    fn poll_list_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> DurableConversionPoll<Vec<Key>> {
        self.with_state_transition(access, |state| {
            let ManagedKeyConversionState::List(state) = state else {
                panic!("key-list wrapper retained scalar conversion state")
            };
            state.poll_in(access, step_budget)
        })
    }

    fn poll_list_optional_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> DurableConversionPoll<Option<Vec<Key>>> {
        self.with_state_transition(access, |state| {
            let ManagedKeyConversionState::List(state) = state else {
                panic!("key-list wrapper retained scalar conversion state")
            };
            state.poll_optional_in(access, step_budget)
        })
    }

    fn with_state_transition<T>(
        &self,
        access: &EvaluationValueAccess<'_>,
        transition: impl FnOnce(&mut ManagedKeyConversionState) -> RegionalConversionPoll<T>,
    ) -> DurableConversionPoll<T> {
        assert!(
            access.values().admits_root(&self.root),
            "key conversion checkpoint must share the evaluator value domain"
        );
        let owner = access.values().project_root(&self.root);
        let cell = access.values().get(&self.root);
        let mut state = match cell.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return DurableConversionPoll::Failed(access.values().root_runtime_failure(
                    Arc::new(EvaluationFailure::message(
                        "managed key conversion state was poisoned by an earlier unwind",
                    )),
                ));
            }
        };
        // SAFETY: the registered wrapper root keeps `owner` live in this
        // exact access region. The cell mutex excludes another transition,
        // and both visitors exhaustively report every raw value and nested
        // regional-WHNF edge before and after the mutation.
        let result = unsafe {
            access.values().with_managed_edge_state_transition(
                &owner,
                &mut *state,
                ManagedKeyConversionState::trace_managed_edges,
                ManagedKeyConversionState::trace_managed_edges,
                transition,
            )
        };
        match result {
            RegionalConversionPoll::Ready(value) => DurableConversionPoll::Ready(value),
            RegionalConversionPoll::Boundary(request) => DurableConversionPoll::Boundary(request),
            RegionalConversionPoll::Yielded => DurableConversionPoll::Yielded,
            RegionalConversionPoll::Failed(failure) => {
                DurableConversionPoll::Failed(access.values().root_runtime_failure(failure))
            }
        }
    }
}

impl RegionalKeyConversion {
    pub(in crate::eval) fn new(
        access: &EvaluationValueAccess<'_>,
        value: Value,
        source_owner: Option<LazyId>,
    ) -> Self {
        Self {
            state: RegionalKeyConversionState::Demand(regional_whnf(access, value, source_owner)),
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalConversionPoll<Key> {
        match self.poll_optional_in(access, step_budget) {
            RegionalConversionPoll::Ready(Some(key)) => RegionalConversionPoll::Ready(key),
            RegionalConversionPoll::Ready(None) => RegionalConversionPoll::Failed(Arc::new(
                EvaluationFailure::message("dictionary keys must evaluate to keyable values"),
            )),
            RegionalConversionPoll::Boundary(request) => RegionalConversionPoll::Boundary(request),
            RegionalConversionPoll::Yielded => RegionalConversionPoll::Yielded,
            RegionalConversionPoll::Failed(failure) => RegionalConversionPoll::Failed(failure),
        }
    }

    fn poll_optional_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalConversionPoll<Option<Key>> {
        match &mut self.state {
            RegionalKeyConversionState::Demand(computation) => {
                let value = match poll_regional_whnf(computation, access, step_budget) {
                    RegionalConversionPoll::Ready(value) => value,
                    RegionalConversionPoll::Boundary(request) => {
                        return RegionalConversionPoll::Boundary(request);
                    }
                    RegionalConversionPoll::Yielded => return RegionalConversionPoll::Yielded,
                    RegionalConversionPoll::Failed(failure) => {
                        return RegionalConversionPoll::Failed(failure);
                    }
                };
                match classify_regional_key_value(access, value) {
                    RegionalClassifiedKeyValue::Ready(key) => {
                        RegionalConversionPoll::Ready(Some(key))
                    }
                    RegionalClassifiedKeyValue::List(value) => {
                        self.state = RegionalKeyConversionState::List(Box::new(
                            RegionalKeyList::from_ready(access, value, self.source_owner),
                        ));
                        RegionalConversionPoll::Yielded
                    }
                    RegionalClassifiedKeyValue::Dict(members) => {
                        self.state = RegionalKeyConversionState::Dict(RegionalDictConversion {
                            members,
                            next: 0,
                            converted: Vec::new(),
                            child: None,
                        });
                        RegionalConversionPoll::Yielded
                    }
                    RegionalClassifiedKeyValue::Invalid => RegionalConversionPoll::Ready(None),
                }
            }
            RegionalKeyConversionState::Dict(dict) => {
                if let Some(child) = &mut dict.child {
                    return match child.poll_optional_in(access, step_budget) {
                        RegionalConversionPoll::Ready(Some(value)) => {
                            let (key, _) = &dict.members[dict.next - 1];
                            if !matches!(&value, Key::Dict(entries) if entries.is_empty()) {
                                dict.converted.push((key.clone(), value));
                            }
                            dict.child = None;
                            RegionalConversionPoll::Yielded
                        }
                        RegionalConversionPoll::Ready(None) => RegionalConversionPoll::Ready(None),
                        RegionalConversionPoll::Boundary(request) => {
                            RegionalConversionPoll::Boundary(request)
                        }
                        RegionalConversionPoll::Yielded => RegionalConversionPoll::Yielded,
                        RegionalConversionPoll::Failed(failure) => {
                            RegionalConversionPoll::Failed(failure)
                        }
                    };
                }
                let Some((_, value)) = dict.members.get(dict.next) else {
                    return RegionalConversionPoll::Ready(Some(Key::Dict(Arc::from(
                        std::mem::take(&mut dict.converted),
                    ))));
                };
                dict.next += 1;
                dict.child = Some(Box::new(Self::new(
                    access,
                    access.values().duplicate_value(value),
                    self.source_owner,
                )));
                RegionalConversionPoll::Yielded
            }
            RegionalKeyConversionState::List(list) => {
                match list.poll_optional_in(access, step_budget) {
                    RegionalConversionPoll::Ready(Some(items)) => {
                        RegionalConversionPoll::Ready(Some(Key::List(Arc::from(items))))
                    }
                    RegionalConversionPoll::Ready(None) => RegionalConversionPoll::Ready(None),
                    RegionalConversionPoll::Boundary(request) => {
                        RegionalConversionPoll::Boundary(request)
                    }
                    RegionalConversionPoll::Yielded => RegionalConversionPoll::Yielded,
                    RegionalConversionPoll::Failed(failure) => {
                        RegionalConversionPoll::Failed(failure)
                    }
                }
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.state {
            RegionalKeyConversionState::Demand(work) => work.trace_managed_edges(visitor),
            RegionalKeyConversionState::Dict(dict) => dict.trace_managed_edges(visitor),
            RegionalKeyConversionState::List(list) => list.trace_managed_edges(visitor),
        }
    }
}

impl RegionalDictConversion {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for (_, value) in &self.members {
            trace_compatibility_value_managed_edges(value, visitor);
        }
        if let Some(child) = &self.child {
            child.trace_managed_edges(visitor);
        }
    }
}

impl RegionalKeyList {
    pub(in crate::eval) fn new(
        access: &EvaluationValueAccess<'_>,
        value: Value,
        source_owner: Option<LazyId>,
    ) -> Self {
        Self {
            source: Some(regional_whnf(access, value, source_owner)),
            lists: Vec::new(),
            chunk: None,
            chunk_suffix: None,
            child: None,
            converted: Vec::new(),
            source_owner,
        }
    }

    pub(in crate::eval) fn from_ready(
        _access: &EvaluationValueAccess<'_>,
        value: Value,
        source_owner: Option<LazyId>,
    ) -> Self {
        Self {
            source: None,
            lists: vec![value],
            chunk: None,
            chunk_suffix: None,
            child: None,
            converted: Vec::new(),
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalConversionPoll<Vec<Key>> {
        match self.poll_optional_in(access, step_budget) {
            RegionalConversionPoll::Ready(Some(keys)) => RegionalConversionPoll::Ready(keys),
            RegionalConversionPoll::Ready(None) => RegionalConversionPoll::Failed(Arc::new(
                EvaluationFailure::message("dictionary keys must evaluate to keyable values"),
            )),
            RegionalConversionPoll::Boundary(request) => RegionalConversionPoll::Boundary(request),
            RegionalConversionPoll::Yielded => RegionalConversionPoll::Yielded,
            RegionalConversionPoll::Failed(failure) => RegionalConversionPoll::Failed(failure),
        }
    }

    pub(in crate::eval) fn poll_optional_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalConversionPoll<Option<Vec<Key>>> {
        if let Some(child) = &mut self.child {
            return match child.poll_optional_in(access, step_budget) {
                RegionalConversionPoll::Ready(Some(key)) => {
                    self.converted.push(key);
                    self.child = None;
                    RegionalConversionPoll::Yielded
                }
                RegionalConversionPoll::Ready(None) => RegionalConversionPoll::Ready(None),
                RegionalConversionPoll::Boundary(request) => {
                    RegionalConversionPoll::Boundary(request)
                }
                RegionalConversionPoll::Yielded => RegionalConversionPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => RegionalConversionPoll::Failed(failure),
            };
        }

        if let Some(computation) = &mut self.source {
            let value = match poll_regional_whnf(computation, access, step_budget) {
                RegionalConversionPoll::Ready(value) => value,
                RegionalConversionPoll::Boundary(request) => {
                    return RegionalConversionPoll::Boundary(request);
                }
                RegionalConversionPoll::Yielded => return RegionalConversionPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => {
                    return RegionalConversionPoll::Failed(failure);
                }
            };
            let list = match value_as_regional_list(access, value, "path-list operand", false) {
                Ok(list) => list,
                Err(error) => {
                    return RegionalConversionPoll::Failed(error.into_permanent_failure());
                }
            };
            self.source = None;
            self.lists.push(list);
            return RegionalConversionPoll::Yielded;
        }

        if let Some(computation) = &mut self.chunk {
            let value = match poll_regional_whnf(computation, access, step_budget) {
                RegionalConversionPoll::Ready(value) => value,
                RegionalConversionPoll::Boundary(request) => {
                    return RegionalConversionPoll::Boundary(request);
                }
                RegionalConversionPoll::Yielded => return RegionalConversionPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => {
                    return RegionalConversionPoll::Failed(failure);
                }
            };
            let list = match value_as_regional_list(access, value, "lazy list chunk", true) {
                Ok(list) => list,
                Err(error) => {
                    return RegionalConversionPoll::Failed(error.into_permanent_failure());
                }
            };
            self.chunk = None;
            if let Some(suffix) = self.chunk_suffix.take() {
                self.lists.push(suffix);
            }
            self.lists.push(list);
            return RegionalConversionPoll::Yielded;
        }

        let Some(list) = self.lists.pop() else {
            return RegionalConversionPoll::Ready(Some(std::mem::take(&mut self.converted)));
        };
        let Value::List(list) = list else {
            unreachable!("regional key-list work must retain list values")
        };
        let step = list.pop_front_step_by(
            &mut |value| access.values().duplicate_value(value),
            &mut |thunk| thunk.duplicate_as_value_in(access.values()),
        );
        match step {
            ListFrontStep::Empty => RegionalConversionPoll::Yielded,
            ListFrontStep::Item { item, tail } => {
                self.lists.push(Value::List(tail));
                match item {
                    ListItem::Byte(byte) => {
                        self.converted.push(Key::Number(Number::from_u8(byte)));
                    }
                    ListItem::Value(value) => {
                        self.child = Some(Box::new(RegionalKeyConversion::new(
                            access,
                            value,
                            self.source_owner,
                        )));
                    }
                }
                RegionalConversionPoll::Yielded
            }
            ListFrontStep::Deferred { deferred, suffix } => {
                self.chunk = Some(regional_whnf(access, deferred, self.source_owner));
                self.chunk_suffix = Some(Value::List(suffix));
                RegionalConversionPoll::Yielded
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(source) = &self.source {
            source.trace_managed_edges(visitor);
        }
        for list in &self.lists {
            trace_compatibility_value_managed_edges(list, visitor);
        }
        if let Some(chunk) = &self.chunk {
            chunk.trace_managed_edges(visitor);
        }
        if let Some(suffix) = &self.chunk_suffix {
            trace_compatibility_value_managed_edges(suffix, visitor);
        }
        if let Some(child) = &self.child {
            child.trace_managed_edges(visitor);
        }
    }
}

impl ManagedKeyConversionState {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match self {
            Self::Key(state) => state.trace_managed_edges(visitor),
            Self::List(state) => state.trace_managed_edges(visitor),
        }
    }
}

fn regional_whnf(
    access: &EvaluationValueAccess<'_>,
    value: Value,
    source_owner: Option<LazyId>,
) -> RegionalWhnfWork {
    let work = RegionalWhnfWork::from_focus(access, value);
    match source_owner {
        Some(source_owner) => work.with_source_owner(source_owner),
        None => work,
    }
}

fn poll_regional_whnf(
    work: &mut RegionalWhnfWork,
    access: &EvaluationValueAccess<'_>,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> RegionalConversionPoll<Value> {
    match drive_regional_in_place(access, work, step_budget, reduce_semantic_shell) {
        RegionalWhnfStatus::Ready(value) => RegionalConversionPoll::Ready(value),
        RegionalWhnfStatus::Boundary(request) => RegionalConversionPoll::Boundary(request),
        RegionalWhnfStatus::Yielded => RegionalConversionPoll::Yielded,
        RegionalWhnfStatus::Failed(failure) => RegionalConversionPoll::Failed(failure),
    }
}

fn interpret_durable_conversion<T>(
    result: DurableConversionPoll<T>,
    context: &EvalContext,
) -> ConversionPoll<T> {
    match result {
        DurableConversionPoll::Ready(value) => ConversionPoll::Ready(value),
        DurableConversionPoll::Boundary(request) => {
            let poll = match request {
                RegionalBoundaryRequest::Dependency(dependency) => WhnfPoll::Pending(dependency),
                RegionalBoundaryRequest::Deferred(deferred) => WhnfPoll::Deferred(deferred),
                RegionalBoundaryRequest::External(boundary) => WhnfPoll::External(boundary),
            };
            match interpret_whnf_poll(poll, context) {
                WhnfOwnerPoll::Pending(dependency) => ConversionPoll::Pending(dependency),
                WhnfOwnerPoll::Yielded => ConversionPoll::Yielded,
                WhnfOwnerPoll::Failed(failure) => ConversionPoll::Failed(failure),
                WhnfOwnerPoll::External(boundary) => {
                    unreachable!("key conversion produced an external {boundary:?} boundary")
                }
                WhnfOwnerPoll::Ready(_) => {
                    unreachable!("a semantic boundary cannot produce an immediate value")
                }
            }
        }
        DurableConversionPoll::Yielded => ConversionPoll::Yielded,
        DurableConversionPoll::Failed(failure) => ConversionPoll::Failed(failure),
    }
}

fn classify_regional_key_value(
    access: &EvaluationValueAccess<'_>,
    value: Value,
) -> RegionalClassifiedKeyValue {
    match value {
        Value::Atom(atom) => RegionalClassifiedKeyValue::Ready(Key::Atom(atom)),
        Value::Number(number) => RegionalClassifiedKeyValue::Ready(Key::Number(number)),
        Value::Binary(bytes) => RegionalClassifiedKeyValue::Ready(Key::Binary(bytes)),
        value @ Value::List(_) => RegionalClassifiedKeyValue::List(value),
        Value::Dict(dict) => RegionalClassifiedKeyValue::Dict(
            dict.iter()
                .map(|(key, value)| (key.clone(), access.values().duplicate_value(value)))
                .collect(),
        ),
        Value::Builtin(_)
        | Value::PartialBuiltin(_)
        | Value::Function(_)
        | Value::Net(_)
        | Value::Lazy(_)
        | Value::Promised(_)
        | Value::Metadata(_)
        | Value::Opaque(_) => RegionalClassifiedKeyValue::Invalid,
    }
}

fn value_as_regional_list(
    _access: &EvaluationValueAccess<'_>,
    value: Value,
    subject: &str,
    allow_binary: bool,
) -> Result<Value, EvaluationHalt> {
    match value {
        Value::Binary(bytes) if allow_binary => Ok(Value::List(List::from_bytes(bytes))),
        value @ Value::List(_) => Ok(value),
        _other if subject == "path-list operand" => Err(EvaluationHalt::new(
            "path-list operand must evaluate to a list value",
        )),
        other => Err(EvaluationHalt::new(format!(
            "lazy list chunk must evaluate to a list or binary value, got {other:?}"
        ))),
    }
}

// SAFETY: the state visitor is compile-exhaustive over every raw `Value`,
// regional WHNF child, and recursive converter. Collection runs only after
// mutator quiescence, so an unpoisoned busy mutex is an invariant failure.
unsafe impl Trace for ManagedKeyConversionCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("managed key conversion state must be quiescent during tracing")
            }
        };
        state.trace_managed_edges(visitor);
    }
}

// SAFETY: direct destruction releases only passive compatibility values,
// regional WHNF state, scalar keys, and ordinary collections. It invokes no
// runtime, evaluator, scheduler, host, or diagnostic capability.
unsafe impl ManagedFamily for ManagedKeyConversionCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "temporary managed key conversion checkpoint",
        "src/eval/access_machine.rs",
        "no direct Drop implementation",
        "regional conversion and WHNF state destroy passively",
    );
}

fn select_dict_member_in(
    access: &EvaluationValueAccess<'_>,
    current: &Value,
    key: &Key,
) -> Result<Value, EvaluationHalt> {
    let Value::Dict(dict) = current else {
        return Err(EvaluationHalt::new("value access base is not a dictionary"));
    };
    Ok(dict
        .get(key)
        .map(|value| access.values().duplicate_value(value))
        .unwrap_or_else(|| Value::Dict(Dict::new_sync())))
}

fn root_message(context: &EvaluatorStepContext<'_>, message: &str) -> RuntimeFailureRoot {
    context.root_failure(Arc::new(EvaluationFailure::message(message)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::{CoreValueFactory, LazyValue, ListThunk, PromisedValue};
    use crate::evaluation::EvaluationStepBudget;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    fn access_value(
        context: &EvalContext,
        path: impl Into<Arc<[CoreDataKey]>>,
        arguments: Vec<Value>,
    ) -> Value {
        Value::Lazy(LazyValue::from_access(
            context.values(),
            path.into(),
            Arc::from(arguments),
        ))
    }

    fn poll_key(
        machine: &mut KeyConversionMachine,
        context: &EvalContext,
        allowance: usize,
    ) -> ConversionPoll<Key> {
        let poll = EvaluationPollContext::for_context(context);
        poll.evaluate(context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                context,
                &mut EvaluationStepBudget::new(allowance),
            )
        })
    }

    fn poll_key_list(
        machine: &mut KeyListMachine,
        context: &EvalContext,
        allowance: usize,
    ) -> ConversionPoll<Vec<Key>> {
        let poll = EvaluationPollContext::for_context(context);
        poll.evaluate(context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                context,
                &mut EvaluationStepBudget::new(allowance),
            )
        })
    }

    #[test]
    fn shared_key_converter_uses_one_managed_root_and_traces_nested_regional_state() {
        let context = context();
        let member = Key::atom_from_text("member");
        let nested = Value::List(List::from_values(vec![
            Value::Number(1.into()),
            Value::Number(2.into()),
        ]));
        let input = context.values().construct_runtime_value_root(|_| {
            Value::Dict(Dict::new_sync().insert(member.clone(), nested))
        });
        let registrations = context.values().managed_root_registrations_for_test();
        let mut machine = KeyConversionMachine::new(input, None);

        let result = loop {
            match poll_key(&mut machine, &context, 1) {
                ConversionPoll::Ready(key) => break key,
                ConversionPoll::Yielded => {
                    context
                        .values()
                        .collect_managed_for_test()
                        .expect("the regional key state should remain traced by its wrapper");
                }
                ConversionPoll::Pending(_) => {
                    panic!("strict nested key conversion must not block")
                }
                ConversionPoll::Failed(failure) => {
                    panic!("strict nested key conversion failed: {failure:?}")
                }
            }
        };

        assert_eq!(
            result,
            Key::Dict(Arc::from([(
                member,
                Key::List(Arc::from([Key::Number(1.into()), Key::Number(2.into())]))
            )]))
        );
        assert_eq!(
            context.values().managed_root_registrations_for_test(),
            registrations + 1,
            "recursive scalar/list conversion must share one temporary managed cell"
        );
    }

    #[test]
    fn shared_key_list_converter_survives_deferred_collection_with_one_root() {
        let context = context();
        let chunk = PromisedValue::new(context.values(), "regional key-list chunk");
        let input = context.values().construct_runtime_value_root(|_| {
            Value::List(List::concat(
                List::from_values(vec![Value::Number(1.into())]),
                List::from_thunk(ListThunk::Promised(chunk.clone())),
            ))
        });
        let registrations = context.values().managed_root_registrations_for_test();
        let mut machine = KeyListMachine::unowned(input);

        assert!(matches!(
            poll_key_list(&mut machine, &context, 1),
            ConversionPoll::Yielded
        ));
        assert_eq!(
            context.values().managed_root_registrations_for_test(),
            registrations + 1,
            "recursive list/path conversion must share one temporary managed cell"
        );
        context
            .values()
            .collect_managed_for_test()
            .expect("the key-list source must remain traced after promotion");

        loop {
            match poll_key_list(&mut machine, &context, 1) {
                ConversionPoll::Pending(_) => break,
                ConversionPoll::Yielded => {
                    context
                        .values()
                        .collect_managed_for_test()
                        .expect("the key-list prefix must remain traced before its dependency");
                }
                ConversionPoll::Ready(_) => {
                    panic!("an unresolved deferred key-list chunk must not complete")
                }
                ConversionPoll::Failed(failure) => {
                    panic!("the deferred key-list prefix failed: {failure:?}")
                }
            }
        }
        context
            .values()
            .collect_managed_for_test()
            .expect("the exact key-list dependency must remain traced across collection");
        crate::core::set_test_promise(
            context.values(),
            &chunk,
            Value::Binary(bytes::Bytes::from_static(&[2_u8, 3_u8])),
        )
        .expect("the deferred binary key-list chunk should accept its assignment");

        let keys = loop {
            match poll_key_list(&mut machine, &context, 1) {
                ConversionPoll::Ready(keys) => break keys,
                ConversionPoll::Yielded => {
                    context
                        .values()
                        .collect_managed_for_test()
                        .expect("every yielded key-list transition must retain its edges");
                }
                ConversionPoll::Pending(_) => {
                    panic!("an assigned key-list chunk must not return to its promise wait")
                }
                ConversionPoll::Failed(failure) => {
                    panic!("the assigned key-list chunk failed: {failure:?}")
                }
            }
        };
        assert_eq!(
            keys,
            vec![
                Key::Number(1.into()),
                Key::Number(2.into()),
                Key::Number(3.into()),
            ]
        );
    }

    #[test]
    fn scalar_computed_key_resumes_from_its_exact_promise() {
        let context = context();
        let promise = PromisedValue::new(context.values(), "computed access key");
        let base = Value::Dict(
            Dict::new_sync().insert(Key::Number(42.into()), Value::binary_from_text("found")),
        );
        let access = access_value(
            &context,
            [CoreDataKey::Index],
            vec![base, Value::Promised(promise.clone())],
        );
        let Value::Lazy(access_lazy) = &access else {
            unreachable!("computed access fixture must be lazy")
        };
        let access_root = access_lazy.root(context.values());

        let blocked = crate::eval::eval_value(&context, &access)
            .expect_err("the dynamic key must wait on its exact promise");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        context
            .values()
            .collect_managed_for_test()
            .expect("the lazy-owned access checkpoint must survive route loss");
        crate::core::set_test_promise(context.values(), &promise, Value::Number(42.into()))
            .expect("the dynamic key promise should accept its assignment");

        assert_eq!(
            crate::eval::eval_value(&context, &access).expect("computed access should resume"),
            Value::binary_from_text("found")
        );
        drop(access_root);
    }

    #[test]
    fn recursive_dictionary_key_conversion_resumes_at_its_member() {
        let context = context();
        let member = Key::atom_from_text("member");
        let promise = PromisedValue::new(context.values(), "dictionary key member");
        let expected_key = Key::Dict(Arc::from([(member.clone(), Key::Number(7.into()))]));
        let base = Value::Dict(
            Dict::new_sync().insert(expected_key, Value::binary_from_text("recursive")),
        );
        let dynamic =
            Value::Dict(Dict::new_sync().insert(member, Value::Promised(promise.clone())));
        let access = access_value(&context, [CoreDataKey::Index], vec![base, dynamic]);

        crate::eval::eval_value(&context, &access)
            .expect_err("the recursive key member must remain a dependency");
        crate::core::set_test_promise(context.values(), &promise, Value::Number(7.into()))
            .expect("the recursive key promise should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &access).expect("recursive key should resume"),
            Value::binary_from_text("recursive")
        );
    }

    #[test]
    fn computed_path_resumes_after_a_deferred_middle_chunk() {
        let context = context();
        let promise = PromisedValue::new(context.values(), "computed path chunk");
        let base = Value::Dict(Dict::new_sync().insert(
            Key::Number(1.into()),
            Value::Dict(
                Dict::new_sync().insert(Key::Number(2.into()), Value::binary_from_text("path")),
            ),
        ));
        let path = Value::List(List::concat(
            List::from_values(vec![Value::Number(1.into())]),
            List::from_thunk(ListThunk::Promised(promise.clone())),
        ));
        let access = access_value(&context, [CoreDataKey::PathIndex], vec![base, path]);

        crate::eval::eval_value(&context, &access)
            .expect_err("the computed path must wait at its deferred chunk");
        crate::core::set_test_promise(
            context.values(),
            &promise,
            Value::List(List::from_values(vec![Value::Number(2.into())])),
        )
        .expect("the path chunk promise should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &access).expect("computed path should resume"),
            Value::binary_from_text("path")
        );
    }

    #[test]
    fn computed_path_preserves_completed_prefix_base_and_result_across_route_loss() {
        let context = context();
        let prefix_forces = Arc::new(AtomicUsize::new(0));
        let middle = PromisedValue::new(context.values(), "computed path middle chunk");
        let base = PromisedValue::new(context.values(), "computed access base");
        let result = PromisedValue::new(context.values(), "computed access result");
        let result_root = result.root(context.values());
        let observed_prefix_forces = Arc::clone(&prefix_forces);
        let prefix = Value::semantic_thunk(context.values(), "computed path prefix", move |_| {
            observed_prefix_forces.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Number(1.into()))
        });
        let path = Value::List(List::concat(
            List::from_values(vec![prefix]),
            List::from_thunk(ListThunk::Promised(middle.clone())),
        ));
        let access = access_value(
            &context,
            [CoreDataKey::PathIndex],
            vec![Value::Promised(base.clone()), path],
        );
        let Value::Lazy(access_lazy) = &access else {
            unreachable!("computed access fixture must be lazy")
        };
        let access_root = access_lazy.root(context.values());

        crate::eval::eval_value(&context, &access)
            .expect_err("the first route must suspend at the middle path chunk");
        assert_eq!(prefix_forces.load(Ordering::SeqCst), 1);
        context
            .values()
            .collect_managed_for_test()
            .expect("the edge-owned access state must survive first-route loss");
        crate::eval::eval_value(&context, &access)
            .expect_err("a later route must resume the exact middle dependency");
        assert_eq!(prefix_forces.load(Ordering::SeqCst), 1);

        crate::core::set_test_promise(
            context.values(),
            &middle,
            Value::List(List::from_values(vec![Value::Number(2.into())])),
        )
        .expect("the middle path promise should accept its assignment");
        crate::eval::eval_value(&context, &access)
            .expect_err("the completed path must next demand the selected base");
        context
            .values()
            .collect_managed_for_test()
            .expect("the completed path must survive while its base is pending");
        crate::core::set_test_promise(
            context.values(),
            &base,
            Value::Dict(Dict::new_sync().insert(
                Key::Number(1.into()),
                Value::Dict(
                    Dict::new_sync().insert(Key::Number(2.into()), Value::Promised(result.clone())),
                ),
            )),
        )
        .expect("the selected base promise should accept its assignment");
        crate::eval::eval_value(&context, &access)
            .expect_err("the selected value must be demanded before completion");
        context
            .values()
            .collect_managed_for_test()
            .expect("the WHNF handoff must survive while its result is pending");
        crate::core::set_test_promise(context.values(), &result, Value::binary_from_text("done"))
            .expect("the selected result promise should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &access)
                .expect("the resumed route should demand the selected result"),
            Value::binary_from_text("done")
        );
        assert_eq!(prefix_forces.load(Ordering::SeqCst), 1);
        assert_eq!(
            crate::eval::eval_value(&context, &access)
                .expect("a terminal route must use the access cache"),
            Value::binary_from_text("done")
        );
        assert_eq!(prefix_forces.load(Ordering::SeqCst), 1);
        drop(access_root);
        drop(result_root);
    }

    #[test]
    fn computed_path_source_rejects_binary_but_deferred_chunks_accept_it() {
        let context = context();
        let base = Value::Dict(Dict::new_sync());
        let invalid = access_value(
            &context,
            [CoreDataKey::PathIndex],
            vec![base.clone(), Value::binary_from_text("x")],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &invalid)
                .expect_err("a path operand itself must remain a list")
                .to_string(),
            "path-list operand must evaluate to a list value"
        );

        let key = Key::Number(Number::from_u8(b'x'));
        let base = Value::Dict(Dict::new_sync().insert(key, Value::binary_from_text("byte")));
        let chunk =
            LazyValue::semantic_thunk(context.values(), "deferred binary path chunk", |_| {
                Ok(Value::binary_from_text("x"))
            });
        let path = Value::List(List::from_thunk(ListThunk::Lazy(chunk)));
        let access = access_value(&context, [CoreDataKey::PathIndex], vec![base, path]);
        assert_eq!(
            crate::eval::eval_value(&context, &access)
                .expect("a deferred binary chunk remains a logical list segment"),
            Value::binary_from_text("byte")
        );
    }
}
