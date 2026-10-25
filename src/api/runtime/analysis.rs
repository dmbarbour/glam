//! An analysis of the runtime (`quiescence-is-analyzed`): whether it is
//! quiescent, what each unfinished work waits on, and what is owed. A host
//! may take one at any time; it changes nothing, and what to do with it is
//! the host's.

use std::sync::Arc;

use super::EvaluationRuntime;
use super::readiness::{FailedMachine, RuntimeDeadlockWork, runtime_deadlock_work_from_snapshot};
use crate::api::{Diagnostic, Error, MachinePath, Value, Values};
use crate::diagnostic::Severity;
use crate::evaluation::RuntimeWaitFamilySnapshot;
use crate::runtime::EvaluationRuntimeId;

/// What a waiting work waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeWaitFamily {
    /// It owes nothing: it waits on observed state or input, or for queued
    /// tasks, or has not started, no one demanding it.
    Idle,
    /// It waits on work that still runs, or can, so the runtime is not
    /// quiescent.
    InProgress,
    /// It awaits a value, a promise's or other work's, that nothing in
    /// progress will produce.
    PromiseWait,
    /// It waits for a change after an error, which a change might cure.
    ErredWait,
}

/// One unfinished work, with its family and the work its wait ends at.
#[derive(Clone, Debug)]
pub struct RuntimeWait {
    work: RuntimeDeadlockWork,
    family: RuntimeWaitFamily,
    cause: Option<u64>,
    awaited: bool,
}

impl RuntimeWait {
    pub fn work(&self) -> &RuntimeDeadlockWork {
        &self.work
    }

    pub fn family(&self) -> RuntimeWaitFamily {
        self.family
    }

    /// The work this wait's chain ends at, where that is another's: the
    /// work that awaits nothing more.
    pub fn cause_work_id(&self) -> Option<u64> {
        self.cause
    }

    /// Whether another unfinished work awaits this one, so that it is a
    /// link in that work's wait, such as a lazy value forced on the way.
    pub fn awaited(&self) -> bool {
        self.awaited
    }
}

/// A promise that machines accepted, still pending
/// (`obligations-are-accepted-promises`), with what each acceptor owes.
#[derive(Clone, Debug)]
pub struct PendingObligation {
    promise_id: u64,
    acceptors: Vec<(MachinePath, Diagnostic)>,
}

impl PendingObligation {
    pub fn promise_id(&self) -> u64 {
        self.promise_id
    }

    /// Each acceptor's path from the root, with the diagnostic it accepted
    /// the promise with, by path.
    pub fn acceptors(&self) -> &[(MachinePath, Diagnostic)] {
        &self.acceptors
    }
}

/// A family of unresolved items. By default each fails a batch; a client
/// may mask families, or recognize issues of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeIssueFamily {
    PromiseWait,
    ErredWait,
    PendingObligation,
    MachineInError,
}

/// One unresolved item of an analysis.
#[derive(Clone, Copy, Debug)]
pub enum RuntimeIssue<'a> {
    Wait(&'a RuntimeWait),
    Obligation(&'a PendingObligation),
    Machine(&'a FailedMachine),
}

impl RuntimeIssue<'_> {
    pub fn family(&self) -> RuntimeIssueFamily {
        match self {
            Self::Obligation(_) => RuntimeIssueFamily::PendingObligation,
            Self::Machine(_) => RuntimeIssueFamily::MachineInError,
            Self::Wait(wait) => match wait.family {
                RuntimeWaitFamily::PromiseWait => RuntimeIssueFamily::PromiseWait,
                RuntimeWaitFamily::ErredWait => RuntimeIssueFamily::ErredWait,
                RuntimeWaitFamily::Idle | RuntimeWaitFamily::InProgress => {
                    unreachable!("only unresolved waits are issues")
                }
            },
        }
    }
}

/// The runtime as an analysis found it. Taken while work runs, it is a view
/// of moving state, whose waits may already have moved on; at quiescence
/// its issues are what the runtime is left owing.
#[derive(Clone, Debug)]
pub struct RuntimeAnalysis {
    runtime: EvaluationRuntimeId,
    quiescent: bool,
    waits: Vec<RuntimeWait>,
    obligations: Vec<PendingObligation>,
    failed_machines: Vec<FailedMachine>,
}

impl RuntimeAnalysis {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    /// Whether no work runs or can without action from outside.
    pub fn quiescent(&self) -> bool {
        self.quiescent
    }

    /// Each unfinished work, by its id.
    pub fn waits(&self) -> &[RuntimeWait] {
        &self.waits
    }

    /// The promises machines accepted that are still pending, by promise.
    pub fn obligations(&self) -> &[PendingObligation] {
        &self.obligations
    }

    /// The machines in error, flagged or failed anonymous roots, by path.
    pub fn failed_machines(&self) -> &[FailedMachine] {
        &self.failed_machines
    }

    /// What is unresolved: the waits that owe something, the pending
    /// obligations, then the machines in error. A promise wait counts once, at the head of its chain, which
    /// names where the chain ends; the links another work awaits are left
    /// out. An erred wait counts wherever it is.
    pub fn issues(&self) -> Vec<RuntimeIssue<'_>> {
        self.waits
            .iter()
            .filter(|wait| match wait.family {
                RuntimeWaitFamily::Idle | RuntimeWaitFamily::InProgress => false,
                RuntimeWaitFamily::PromiseWait => !wait.awaited,
                RuntimeWaitFamily::ErredWait => true,
            })
            .map(RuntimeIssue::Wait)
            .chain(self.obligations.iter().map(RuntimeIssue::Obligation))
            .chain(self.failed_machines.iter().map(RuntimeIssue::Machine))
            .collect()
    }
}

impl EvaluationRuntime {
    /// Analyzes the runtime as it stands (`quiescence-is-analyzed`): at any
    /// time, changing nothing. It fails only on a poisoned runtime.
    pub fn analysis(&self) -> Result<RuntimeAnalysis, Error> {
        if self.state.shared_resources.mutation_admission.is_poisoned() {
            return Err(Error::new(format!(
                "evaluation runtime {} is poisoned by an internal panic",
                self.id().get()
            )));
        }
        let coordinator = self.state.work.runtime_analysis_snapshot();
        let failed_roots = self.state.work.failed_roots();
        let (delivering, mut failures, accepted) = {
            let state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            (
                !state.events.outputs.records.is_empty(),
                state.events.flags.standing(),
                state.events.accepted.pending(),
            )
        };
        let values = self.values();
        let core = values.core();
        // An annotation task's failure flags its anonymous root
        // (`reflection-tasks-are-machine-tasks`).
        failures.extend(failed_roots.iter().map(|(task, failure)| {
            (
                crate::reflection::anonymous_root(*task),
                failure.diagnostic(core),
            )
        }));
        failures.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(RuntimeAnalysis {
            runtime: self.id(),
            quiescent: coordinator.quiescent && !delivering,
            waits: coordinator
                .waits
                .into_iter()
                .map(|wait| RuntimeWait {
                    work: runtime_deadlock_work_from_snapshot(core, wait.work),
                    family: match wait.family {
                        RuntimeWaitFamilySnapshot::Idle => RuntimeWaitFamily::Idle,
                        RuntimeWaitFamilySnapshot::InProgress => RuntimeWaitFamily::InProgress,
                        RuntimeWaitFamilySnapshot::PromiseWait => RuntimeWaitFamily::PromiseWait,
                        RuntimeWaitFamilySnapshot::ErredWait => RuntimeWaitFamily::ErredWait,
                    },
                    cause: wait.cause.map(|work| work.get()),
                    awaited: wait.awaited,
                })
                .collect(),
            obligations: accepted
                .into_iter()
                .map(|(promise, acceptors)| PendingObligation {
                    promise_id: promise.get(),
                    acceptors: acceptors
                        .into_iter()
                        .map(|(path, diagnostic)| {
                            (
                                MachinePath::from(path),
                                Diagnostic::from_parts(
                                    &Values::from_core_factory(core.clone()),
                                    None,
                                    Severity::Error,
                                    Value::from_runtime_root(diagnostic),
                                    None,
                                    None,
                                    Arc::from(crate::api::diagnostics::EMISSION_SUMMARY),
                                ),
                            )
                        })
                        .collect(),
                })
                .collect(),
            failed_machines: failures
                .into_iter()
                .map(|(path, diagnostic)| FailedMachine::new(self.id(), core, path, &diagnostic))
                .collect(),
        })
    }
}
