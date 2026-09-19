//! Typed managed checkpoints retained by lazy producer identities.
//!
//! The carrier is deliberately a concrete sum of typed GC edges. Core lazy
//! storage may retain, duplicate, and trace it, but only evaluator-owned code
//! can select a variant or inspect its state. Adding a producer family must
//! therefore add one explicit trace arm without introducing erased payloads,
//! boxes, or a second runtime trace contract.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, TryLockError};

use glam_gc::{Gc, Trace, UnsupportedLayout, Visitor};

use crate::core::{
    EvaluationFailure, HostCallProducer, ManagedDropRecord, ManagedFamily, RuntimeValueAccess,
    Value, managed_slot_extent, trace_compatibility_value_managed_edges,
};

use super::access_machine::AccessMachine;
use super::net::NetWhnfMachine;
use super::whnf::managed_state::ManagedLazyCheckpointCell;

pub(in crate::eval) struct ManagedNetWhnfCheckpointCell {
    state: Mutex<ManagedNetWhnfCheckpointState>,
}

pub(in crate::eval) struct ManagedAccessCheckpointCell {
    state: Mutex<AccessMachine>,
}

struct ManagedNetWhnfCheckpointState {
    machine: NetWhnfMachine,
    semantic_in_flight: bool,
}

pub(in crate::eval) struct ManagedHostCallCheckpointCell {
    state: Mutex<ManagedHostCallCheckpointState>,
}

enum ManagedHostCallCheckpointState {
    /// The callback is either about to run or was interrupted by unwind. Only
    /// the route which installed this checkpoint receives invocation
    /// authority; a later route must never replay it.
    Invoking(Arc<HostCallProducer>),
    After(Result<Value, Arc<EvaluationFailure>>),
}

pub(in crate::eval) enum HostCallCheckpointObservation {
    Invoking,
    After(Result<Value, Arc<EvaluationFailure>>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::eval) enum ManagedLazyCheckpointKindTag {
    Whnf,
    HostCall,
    NetWhnf,
    Access,
}

pub(crate) struct ManagedLazyCheckpointEdge(ManagedLazyCheckpointKind);

enum ManagedLazyCheckpointKind {
    Whnf(Gc<ManagedLazyCheckpointCell>),
    HostCall(Gc<ManagedHostCallCheckpointCell>),
    NetWhnf(Gc<ManagedNetWhnfCheckpointCell>),
    Access(Gc<ManagedAccessCheckpointCell>),
}

impl ManagedLazyCheckpointEdge {
    pub(in crate::eval) fn from_whnf(edge: Gc<ManagedLazyCheckpointCell>) -> Self {
        Self(ManagedLazyCheckpointKind::Whnf(edge))
    }

    pub(in crate::eval) fn into_whnf(self) -> Gc<ManagedLazyCheckpointCell> {
        match self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => edge,
            ManagedLazyCheckpointKind::HostCall(_) => {
                panic!("a host-call checkpoint cannot be projected as WHNF state")
            }
            ManagedLazyCheckpointKind::NetWhnf(_) => {
                panic!("a net-WHNF checkpoint cannot be projected as ordinary WHNF state")
            }
            ManagedLazyCheckpointKind::Access(_) => {
                panic!("an access checkpoint cannot be projected as ordinary WHNF state")
            }
        }
    }

    pub(in crate::eval) fn kind(&self) -> ManagedLazyCheckpointKindTag {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(_) => ManagedLazyCheckpointKindTag::Whnf,
            ManagedLazyCheckpointKind::HostCall(_) => ManagedLazyCheckpointKindTag::HostCall,
            ManagedLazyCheckpointKind::NetWhnf(_) => ManagedLazyCheckpointKindTag::NetWhnf,
            ManagedLazyCheckpointKind::Access(_) => ManagedLazyCheckpointKindTag::Access,
        }
    }

    pub(crate) fn same_checkpoint_in(
        &self,
        other: &Self,
        authority: &RuntimeValueAccess<'_>,
    ) -> bool {
        match (&self.0, &other.0) {
            (ManagedLazyCheckpointKind::Whnf(left), ManagedLazyCheckpointKind::Whnf(right)) => {
                authority.same_edge(left, right)
            }
            (
                ManagedLazyCheckpointKind::HostCall(left),
                ManagedLazyCheckpointKind::HostCall(right),
            ) => authority.same_edge(left, right),
            (
                ManagedLazyCheckpointKind::NetWhnf(left),
                ManagedLazyCheckpointKind::NetWhnf(right),
            ) => authority.same_edge(left, right),
            (ManagedLazyCheckpointKind::Access(left), ManagedLazyCheckpointKind::Access(right)) => {
                authority.same_edge(left, right)
            }
            _ => false,
        }
    }

    pub(in crate::eval) fn duplicate_whnf_in(
        &self,
        authority: &RuntimeValueAccess<'_>,
    ) -> Option<Gc<ManagedLazyCheckpointCell>> {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => Some(authority.duplicate_edge(edge)),
            ManagedLazyCheckpointKind::HostCall(_) => None,
            ManagedLazyCheckpointKind::NetWhnf(_) => None,
            ManagedLazyCheckpointKind::Access(_) => None,
        }
    }

    pub(in crate::eval) fn allocate_host_call_in(
        authority: &RuntimeValueAccess<'_>,
        producer: Arc<HostCallProducer>,
    ) -> Result<Self, UnsupportedLayout> {
        let allocator = authority.allocator::<ManagedHostCallCheckpointCell>()?;
        Ok(Self(ManagedLazyCheckpointKind::HostCall(allocator.alloc(
            ManagedHostCallCheckpointCell {
                state: Mutex::new(ManagedHostCallCheckpointState::Invoking(producer)),
            },
        ))))
    }

    pub(in crate::eval) fn allocate_net_whnf_in(
        authority: &crate::evaluation::EvaluationValueAccess<'_>,
        machine: NetWhnfMachine,
    ) -> Result<Self, UnsupportedLayout> {
        let allocator = authority
            .values()
            .allocator::<ManagedNetWhnfCheckpointCell>()?;
        Ok(Self(ManagedLazyCheckpointKind::NetWhnf(allocator.alloc(
            ManagedNetWhnfCheckpointCell {
                state: Mutex::new(ManagedNetWhnfCheckpointState {
                    machine,
                    semantic_in_flight: false,
                }),
            },
        ))))
    }

    pub(in crate::eval) fn allocate_access_in(
        authority: &crate::evaluation::EvaluationValueAccess<'_>,
        machine: AccessMachine,
    ) -> Result<Self, UnsupportedLayout> {
        let allocator = authority
            .values()
            .allocator::<ManagedAccessCheckpointCell>()?;
        Ok(Self(ManagedLazyCheckpointKind::Access(allocator.alloc(
            ManagedAccessCheckpointCell {
                state: Mutex::new(machine),
            },
        ))))
    }

    pub(in crate::eval) fn with_access_transition_in<R>(
        &self,
        authority: &crate::evaluation::EvaluationValueAccess<'_>,
        transition: impl FnOnce(&mut AccessMachine) -> R,
    ) -> R {
        let ManagedLazyCheckpointKind::Access(edge) = &self.0 else {
            panic!("only an access checkpoint has computed-access state")
        };
        let cell = authority.values().get_edge(edge);
        let mut state = cell
            .state
            .lock()
            .expect("managed access checkpoint was poisoned");
        // SAFETY: the owning lazy retains this exact checkpoint edge. The
        // representation mutex excludes another regional transition, and the
        // access machine's exhaustive visitor reports every leaving and
        // adding semantic edge.
        unsafe {
            authority.values().with_managed_edge_state_transition(
                edge,
                &mut *state,
                AccessMachine::trace_managed_edges,
                AccessMachine::trace_managed_edges,
                transition,
            )
        }
    }

    pub(in crate::eval) fn with_net_whnf_transition_in<R>(
        &self,
        authority: &crate::evaluation::EvaluationValueAccess<'_>,
        transition: impl FnOnce(&mut NetWhnfMachine, &mut bool) -> R,
    ) -> R {
        let ManagedLazyCheckpointKind::NetWhnf(edge) = &self.0 else {
            panic!("only a net-WHNF checkpoint has net driver state")
        };
        let cell = authority.values().get_edge(edge);
        let mut state = cell
            .state
            .lock()
            .expect("managed net-WHNF checkpoint was poisoned");
        // SAFETY: the owning lazy retains this exact checkpoint edge. The
        // representation mutex excludes another driver transition, and both
        // visitors report the complete edge-owned request/worklist state.
        unsafe {
            authority.values().with_managed_edge_state_transition(
                edge,
                &mut *state,
                |state, visitor| state.machine.trace_managed_edges(visitor),
                |state, visitor| state.machine.trace_managed_edges(visitor),
                |state| transition(&mut state.machine, &mut state.semantic_in_flight),
            )
        }
    }

    /// Borrows the producer only for the route which installed `Invoking`.
    /// Merely observing this state does not grant replay authority.
    pub(in crate::eval) fn host_call_producer_in(
        &self,
        authority: &RuntimeValueAccess<'_>,
    ) -> Option<Arc<HostCallProducer>> {
        let ManagedLazyCheckpointKind::HostCall(edge) = &self.0 else {
            return None;
        };
        let cell = authority.get_edge(edge);
        let state = cell
            .state
            .lock()
            .expect("managed host-call checkpoint was poisoned");
        match &*state {
            ManagedHostCallCheckpointState::Invoking(producer) => Some(Arc::clone(producer)),
            ManagedHostCallCheckpointState::After(_) => None,
        }
    }

    pub(in crate::eval) fn complete_host_call_in(
        &self,
        authority: &RuntimeValueAccess<'_>,
        result: Result<Value, Arc<EvaluationFailure>>,
    ) {
        let ManagedLazyCheckpointKind::HostCall(edge) = &self.0 else {
            panic!("only a host-call checkpoint can publish a host-call result")
        };
        let cell = authority.get_edge(edge);
        let state = cell
            .state
            .lock()
            .expect("managed host-call checkpoint was poisoned");
        assert!(
            matches!(*state, ManagedHostCallCheckpointState::Invoking(_)),
            "a host callback outcome may be published exactly once"
        );
        let state = RefCell::new(state);
        let proposed = RefCell::new(Some(result));
        // SAFETY: this exact checkpoint edge is live beneath the owning lazy
        // and authorized by this access. Its state mutex excludes another
        // publication; both visitors report the complete leaving and adding
        // semantic graph before the closure replaces it once.
        unsafe {
            authority.with_managed_edge_transition(
                edge,
                |visitor| {
                    trace_host_call_state(&state.borrow(), visitor);
                },
                |visitor| {
                    trace_host_call_result(
                        proposed
                            .borrow()
                            .as_ref()
                            .expect("host-call transition must retain its proposed result"),
                        visitor,
                    );
                },
                || {
                    let result = proposed
                        .borrow_mut()
                        .take()
                        .expect("host-call transition must consume its result once");
                    let prior = std::mem::replace(
                        &mut **state.borrow_mut(),
                        ManagedHostCallCheckpointState::After(result),
                    );
                    drop(prior);
                },
            )
        }
    }

    pub(in crate::eval) fn observe_host_call_in(
        &self,
        authority: &RuntimeValueAccess<'_>,
    ) -> HostCallCheckpointObservation {
        let ManagedLazyCheckpointKind::HostCall(edge) = &self.0 else {
            panic!("only a host-call checkpoint has a host-call observation")
        };
        let state = authority
            .get_edge(edge)
            .state
            .lock()
            .expect("managed host-call checkpoint was poisoned");
        match &*state {
            ManagedHostCallCheckpointState::Invoking(_) => HostCallCheckpointObservation::Invoking,
            ManagedHostCallCheckpointState::After(Ok(value)) => {
                HostCallCheckpointObservation::After(Ok(authority.duplicate_value(value)))
            }
            ManagedHostCallCheckpointState::After(Err(failure)) => {
                HostCallCheckpointObservation::After(Err(Arc::clone(failure)))
            }
        }
    }

    pub(crate) fn trace(&self, visitor: &mut Visitor<'_>) {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => visitor.visit(edge),
            ManagedLazyCheckpointKind::HostCall(edge) => visitor.visit(edge),
            ManagedLazyCheckpointKind::NetWhnf(edge) => visitor.visit(edge),
            ManagedLazyCheckpointKind::Access(edge) => visitor.visit(edge),
        }
    }

    pub(crate) fn duplicate_in(&self, authority: &RuntimeValueAccess<'_>) -> Self {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => Self(ManagedLazyCheckpointKind::Whnf(
                authority.duplicate_edge(edge),
            )),
            ManagedLazyCheckpointKind::HostCall(edge) => Self(ManagedLazyCheckpointKind::HostCall(
                authority.duplicate_edge(edge),
            )),
            ManagedLazyCheckpointKind::NetWhnf(edge) => Self(ManagedLazyCheckpointKind::NetWhnf(
                authority.duplicate_edge(edge),
            )),
            ManagedLazyCheckpointKind::Access(edge) => Self(ManagedLazyCheckpointKind::Access(
                authority.duplicate_edge(edge),
            )),
        }
    }
}

fn trace_host_call_state(state: &ManagedHostCallCheckpointState, visitor: &mut Visitor<'_>) {
    match state {
        ManagedHostCallCheckpointState::Invoking(producer) => {
            producer.trace_managed_edges(visitor);
        }
        ManagedHostCallCheckpointState::After(result) => {
            trace_host_call_result(result, visitor);
        }
    }
}

fn trace_host_call_result(
    result: &Result<Value, Arc<EvaluationFailure>>,
    visitor: &mut Visitor<'_>,
) {
    match result {
        Ok(value) => trace_compatibility_value_managed_edges(value, visitor),
        Err(failure) => failure.visit_direct_values(&mut |value| {
            trace_compatibility_value_managed_edges(value, visitor);
        }),
    }
}

// SAFETY: the state visitor is compile-exhaustive over the invoking producer's
// declared semantic captures and the published value/failure result. The
// collector runs only after mutator quiescence, so an unpoisoned busy mutex is
// an invariant failure. Poison recovery preserves the structurally installed
// state after unwind and must retain all of its edges.
unsafe impl Trace for ManagedHostCallCheckpointCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("managed host-call state must be quiescent during tracing")
            }
        };
        trace_host_call_state(&state, visitor);
    }
}

// SAFETY: direct destruction releases only passive values, failure shells,
// and an external-owner lease. It invokes no callback or runtime operation;
// retired opaque callback owners are drained later by the runtime registry.
unsafe impl ManagedFamily for ManagedHostCallCheckpointCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed host-call checkpoint cell",
        "src/eval/lazy_checkpoint.rs",
        "no direct Drop implementation",
        "values and external-owner leases destroy passively",
    );
}

// SAFETY: collection runs only after mutator quiescence. The unpoisoned mutex
// therefore cannot be busy, and the canonical net-driver visitor reports each
// managed net edge in the request, worklist, and frontier observations.
unsafe impl Trace for ManagedNetWhnfCheckpointCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("managed net-WHNF state must be quiescent during tracing")
            }
        };
        state.machine.trace_managed_edges(visitor);
    }
}

// SAFETY: direct destruction releases only passive net edges, scalar driver
// state, an Arc operation label, and ordinary vectors. It invokes no runtime,
// evaluator, scheduler, host, or diagnostic capability.
unsafe impl ManagedFamily for ManagedNetWhnfCheckpointCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed net-WHNF checkpoint cell",
        "src/eval/lazy_checkpoint.rs",
        "no direct Drop implementation",
        "mutex and edge-owned net driver destroy passively",
    );
}

// SAFETY: the access machine's compile-exhaustive visitor reports every raw
// argument/current value, regional WHNF child, and recursive key/list
// converter. Collection runs only after mutator quiescence, so an unpoisoned
// busy mutex is an invariant failure rather than ordinary contention.
unsafe impl Trace for ManagedAccessCheckpointCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("managed access state must be quiescent during tracing")
            }
        };
        state.trace_managed_edges(visitor);
    }
}

// SAFETY: direct destruction releases only passive compatibility values,
// regional WHNF/converter state, scalar paths, and ordinary collections. It
// invokes no runtime, evaluator, scheduler, host, or diagnostic capability.
unsafe impl ManagedFamily for ManagedAccessCheckpointCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed computed-access checkpoint cell",
        "src/eval/lazy_checkpoint.rs",
        "no direct Drop implementation",
        "mutex and edge-owned access state destroy passively",
    );
}
