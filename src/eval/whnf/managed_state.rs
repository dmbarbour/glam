//! One rooted managed owner for complete resumable-WHNF demand state.
//!
//! W6G.3c establishes this family before production ownership migrates in
//! W6G.3d. The state mutex is a stop-the-world collector baseline: evaluation
//! mutates one bounded callback-free quantum beneath it, while `Trace` runs
//! only after mutator quiescence and therefore must never find an active lock.

use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Mutex, TryLockError};

use glam_gc::{Gc, Root, Trace, UnsupportedLayout, Visitor};

use crate::core::{ManagedDropRecord, ManagedFamily, managed_slot_extent};
use crate::evaluation::EvaluationValueAccess;
use crate::runtime::EvaluationRuntimeId;

use super::{RegionalWhnfState, RegionalWhnfWork, WhnfState};

pub(super) struct ManagedWhnfCell {
    state: Mutex<WhnfState>,
}

/// Interior identity projected from a registered WHNF root beneath access.
struct ManagedWhnfEdge(Gc<ManagedWhnfCell>);

/// Durable owner of one complete canonical WHNF demand state.
///
/// The scalar runtime identity supports access-free routing and diagnostics;
/// the collector root remains the authoritative provenance check whenever the
/// state is projected.
pub(crate) struct ManagedWhnfRoot {
    runtime: EvaluationRuntimeId,
    root: Root<ManagedWhnfCell>,
}

/// Non-escaping view of one rooted WHNF cell under matching evaluator access.
pub(crate) struct ManagedWhnfAccess<'access, 'scope> {
    owner: ManagedWhnfEdge,
    cell: &'access ManagedWhnfCell,
    authority: &'access EvaluationValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedWhnfAccessError {
    RuntimeMismatch,
    Poisoned,
}

impl ManagedWhnfCell {
    fn from_state(state: WhnfState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }
}

impl ManagedWhnfRoot {
    /// Allocates and roots one complete regional state before access closes.
    ///
    /// W6G.3c uses this constructor only in focused foundation tests. W6G.3d
    /// selects the production seed-promotion and structured-construction call
    /// sites rather than opening hidden access here.
    pub(crate) fn from_regional_in(
        access: &EvaluationValueAccess<'_>,
        work: RegionalWhnfWork,
    ) -> Result<Self, UnsupportedLayout> {
        let allocator = access.values().allocator::<ManagedWhnfCell>()?;
        let cell = allocator.alloc(ManagedWhnfCell::from_state(work.0));
        Ok(Self {
            runtime: access.values().runtime_id(),
            root: access.values().root(cell),
        })
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn access<'access, 'scope>(
        &'access self,
        authority: &'access EvaluationValueAccess<'scope>,
    ) -> Result<ManagedWhnfAccess<'access, 'scope>, ManagedWhnfAccessError> {
        if self.runtime != authority.values().runtime_id()
            || !authority.values().admits_root(&self.root)
        {
            return Err(ManagedWhnfAccessError::RuntimeMismatch);
        }
        Ok(ManagedWhnfAccess {
            owner: ManagedWhnfEdge(authority.values().project_root(&self.root)),
            cell: authority.values().get(&self.root),
            authority,
            _thread_bound: PhantomData,
        })
    }
}

impl ManagedWhnfAccess<'_, '_> {
    #[cfg(test)]
    pub(crate) fn inspect<R>(
        &self,
        operation: impl FnOnce(&WhnfState) -> R,
    ) -> Result<R, ManagedWhnfAccessError> {
        let state = self
            .cell
            .state
            .lock()
            .map_err(|_| ManagedWhnfAccessError::Poisoned)?;
        Ok(operation(&state))
    }

    /// Publishes one complete in-place state transition through the collector
    /// gateway while retaining the cell's sole representation lock.
    pub(crate) fn with_state_transition<R>(
        &self,
        transition: impl FnOnce(&mut RegionalWhnfState<'_>) -> R,
    ) -> Result<R, ManagedWhnfAccessError> {
        let mut state = self
            .cell
            .state
            .lock()
            .map_err(|_| ManagedWhnfAccessError::Poisoned)?;
        // SAFETY: the registered root keeps `owner` live in this exact value
        // region. Both visitors report every edge in the canonical state
        // while its sole mutex is held, and `transition` cannot retain its
        // access-bound regional view.
        Ok(unsafe {
            self.authority.values().with_managed_edge_state_transition(
                &self.owner.0,
                &mut *state,
                WhnfState::trace_managed_edges,
                WhnfState::trace_managed_edges,
                |state| {
                    let mut state = state.regional_in(self.authority);
                    transition(&mut state)
                },
            )
        })
    }
}

// SAFETY: `WhnfState::trace_managed_edges` is the compile-exhaustive canonical
// visitor shared with net-owned checkpoints. Reference collection runs only
// after every mutator exits, so an unpoisoned busy mutex is an invariant
// failure rather than ordinary contention. Poison recovery is observational:
// unwind never removes the structurally installed state, and collection must
// continue to retain every edge even when evaluator repolling rejects it.
unsafe impl Trace for ManagedWhnfCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("managed WHNF state must be quiescent during tracing")
            }
        };
        state.trace_managed_edges(visitor);
    }
}

// SAFETY: this type has no direct Drop implementation. Its mutex and
// canonical WHNF state release only passive compatibility values, inert
// managed edges, scalar identities, and ordinary collections. Destruction
// invokes no runtime, evaluator, scheduler, host, or diagnostic capability.
unsafe impl ManagedFamily for ManagedWhnfCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed resumable-WHNF state cell",
        "src/eval/whnf/managed_state.rs",
        "no direct Drop implementation",
        "mutex and canonical WHNF state destroy passively",
    );
}
