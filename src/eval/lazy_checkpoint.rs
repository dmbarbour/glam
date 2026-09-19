//! Typed managed checkpoints retained by lazy producer identities.
//!
//! The carrier is deliberately a concrete sum of typed GC edges. Core lazy
//! storage may retain, duplicate, and trace it, but only evaluator-owned code
//! can select a variant or inspect its state. Adding a producer family must
//! therefore add one explicit trace arm without introducing erased payloads,
//! boxes, or a second runtime trace contract.

use glam_gc::{Gc, Visitor};

use crate::core::RuntimeValueAccess;

use super::whnf::managed_state::ManagedLazyCheckpointCell;

pub(crate) struct ManagedLazyCheckpointEdge(ManagedLazyCheckpointKind);

enum ManagedLazyCheckpointKind {
    Whnf(Gc<ManagedLazyCheckpointCell>),
}

impl ManagedLazyCheckpointEdge {
    pub(in crate::eval) fn from_whnf(edge: Gc<ManagedLazyCheckpointCell>) -> Self {
        Self(ManagedLazyCheckpointKind::Whnf(edge))
    }

    pub(in crate::eval) fn into_whnf(self) -> Gc<ManagedLazyCheckpointCell> {
        match self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => edge,
        }
    }

    pub(in crate::eval) fn duplicate_whnf_in(
        &self,
        authority: &RuntimeValueAccess<'_>,
    ) -> Option<Gc<ManagedLazyCheckpointCell>> {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => Some(authority.duplicate_edge(edge)),
        }
    }

    pub(crate) fn trace(&self, visitor: &mut Visitor<'_>) {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => visitor.visit(edge),
        }
    }

    pub(crate) fn duplicate_in(&self, authority: &RuntimeValueAccess<'_>) -> Self {
        match &self.0 {
            ManagedLazyCheckpointKind::Whnf(edge) => Self(ManagedLazyCheckpointKind::Whnf(
                authority.duplicate_edge(edge),
            )),
        }
    }
}
