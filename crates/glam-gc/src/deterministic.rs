//! Private deterministic-test-hook home.
//!
//! Hooks are compiled only when the private verification feature is selected.
//! They may observe or pause a production transition, but must not alter its
//! semantic result.

use std::sync::{Arc, Mutex};

use crate::{Mutator, Visitor, trace::ErasedGc};

/// Selects the edge sets traversed by a deterministic mutation probe.
///
/// This is a verification-only approximation of future collector policies;
/// it is not a supported downstream collector configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EdgeTransitionObservation {
    /// Traverse neither side, like the current stop-the-world barrier.
    Neither,
    /// Traverse only edges removed by the mutation, like the provisional SATB
    /// barrier.
    Leaving,
    /// Traverse only newly installed edges.
    Adding,
    /// Traverse both sides.
    Both,
}

impl EdgeTransitionObservation {
    pub(crate) fn leaving(self) -> bool {
        matches!(self, Self::Leaving | Self::Both)
    }

    pub(crate) fn adding(self) -> bool {
        matches!(self, Self::Adding | Self::Both)
    }
}

/// One observed owner-qualified edge-set transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EdgeTransitionRecord {
    leaving_edges: usize,
    adding_edges: usize,
}

impl EdgeTransitionRecord {
    /// Returns the number of leaving edges reported by the selected visitor.
    #[must_use]
    pub const fn leaving_edges(self) -> usize {
        self.leaving_edges
    }

    /// Returns the number of adding edges reported by the selected visitor.
    #[must_use]
    pub const fn adding_edges(self) -> usize {
        self.adding_edges
    }
}

/// Retained observations from one heap-local deterministic mutation probe.
#[derive(Clone)]
pub struct EdgeTransitionProbe {
    pub(crate) state: Arc<EdgeTransitionProbeState>,
}

impl EdgeTransitionProbe {
    pub(crate) fn new(observation: EdgeTransitionObservation) -> Self {
        Self {
            state: Arc::new(EdgeTransitionProbeState {
                observation,
                records: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Returns a stable copy of all transitions observed so far.
    #[must_use]
    pub fn records(&self) -> Vec<EdgeTransitionRecord> {
        self.state
            .records
            .lock()
            .expect("edge-transition test probe was poisoned")
            .clone()
    }
}

pub(crate) struct EdgeTransitionProbeState {
    observation: EdgeTransitionObservation,
    records: Mutex<Vec<EdgeTransitionRecord>>,
}

impl EdgeTransitionProbeState {
    pub(crate) fn observe<Leaving, Adding>(
        &self,
        mutator: &Mutator<'_>,
        leaving: &Leaving,
        adding: &Adding,
    ) where
        Leaving: for<'visit> Fn(&mut Visitor<'visit>),
        Adding: for<'visit> Fn(&mut Visitor<'visit>),
    {
        let leaving_edges = self
            .observation
            .leaving()
            .then(|| count_and_validate(mutator, leaving))
            .unwrap_or(0);
        let adding_edges = self
            .observation
            .adding()
            .then(|| count_and_validate(mutator, adding))
            .unwrap_or(0);
        self.records
            .lock()
            .expect("edge-transition test probe was poisoned")
            .push(EdgeTransitionRecord {
                leaving_edges,
                adding_edges,
            });
    }

    pub(crate) fn observe_state_transition<State, Leaving, Adding, Result>(
        &self,
        mutator: &Mutator<'_>,
        state: &mut State,
        leaving: &Leaving,
        adding: &Adding,
        transition: impl FnOnce(&mut State) -> Result,
    ) -> Result
    where
        Leaving: for<'visit> Fn(&State, &mut Visitor<'visit>),
        Adding: for<'visit> Fn(&State, &mut Visitor<'visit>),
    {
        let leaving_edges = self
            .observation
            .leaving()
            .then(|| count_state_and_validate(mutator, state, leaving))
            .unwrap_or(0);
        let result = transition(state);
        let adding_edges = self
            .observation
            .adding()
            .then(|| count_state_and_validate(mutator, state, adding))
            .unwrap_or(0);
        self.records
            .lock()
            .expect("edge-transition test probe was poisoned")
            .push(EdgeTransitionRecord {
                leaving_edges,
                adding_edges,
            });
        result
    }
}

fn count_and_validate<Edges>(mutator: &Mutator<'_>, edges: &Edges) -> usize
where
    Edges: for<'visit> Fn(&mut Visitor<'visit>),
{
    let mut count = 0usize;
    let mut visit = |edge: ErasedGc| {
        mutator.assert_observed_edge_for_test(edge);
        count = count.checked_add(1).expect("observed edge count exhausted");
    };
    edges(&mut Visitor::new(&mut visit));
    count
}

fn count_state_and_validate<State, Edges>(
    mutator: &Mutator<'_>,
    state: &State,
    edges: &Edges,
) -> usize
where
    Edges: for<'visit> Fn(&State, &mut Visitor<'visit>),
{
    let mut count = 0usize;
    let mut visit = |edge: ErasedGc| {
        mutator.assert_observed_edge_for_test(edge);
        count = count.checked_add(1).expect("observed edge count exhausted");
    };
    edges(state, &mut Visitor::new(&mut visit));
    count
}
