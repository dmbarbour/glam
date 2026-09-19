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

use super::whnf::managed_state::ManagedLazyCheckpointCell;

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
}

pub(crate) struct ManagedLazyCheckpointEdge(ManagedLazyCheckpointKind);

enum ManagedLazyCheckpointKind {
    Whnf(Gc<ManagedLazyCheckpointCell>),
    HostCall(Gc<ManagedHostCallCheckpointCell>),
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
        }
    }

    pub(in crate::eval) fn kind(&self) -> ManagedLazyCheckpointKindTag {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(_) => ManagedLazyCheckpointKindTag::Whnf,
            ManagedLazyCheckpointKind::HostCall(_) => ManagedLazyCheckpointKindTag::HostCall,
        }
    }

    pub(in crate::eval) fn duplicate_whnf_in(
        &self,
        authority: &RuntimeValueAccess<'_>,
    ) -> Option<Gc<ManagedLazyCheckpointCell>> {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => Some(authority.duplicate_edge(edge)),
            ManagedLazyCheckpointKind::HostCall(_) => None,
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
