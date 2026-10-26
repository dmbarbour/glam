//! An analysis of the runtime (`quiescence-is-analyzed`): whether it is
//! quiescent, what each unfinished work waits on, and what is owed. A host
//! may take one at any time; it changes nothing, and what to do with it is
//! the host's.

use std::sync::Arc;

use super::readiness::{FailedMachine, RuntimeWork, runtime_work_from_snapshot};
use super::{
    EvaluationRuntime, RuntimeDeliveryFailure, RuntimeMaintenanceFailure, maintenance_failure,
};
use crate::api::{Diagnostic, Error, MachinePath, Value, Values};
use crate::core::Key;
use crate::diagnostic::Severity;
use crate::evaluation::{RuntimeWaitFamilySnapshot, WaitMachineSnapshot};
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

/// One unfinished work, with its family, the work its wait ends at, and
/// the machine it runs on.
#[derive(Clone, Debug)]
pub struct RuntimeWait {
    work: RuntimeWork,
    family: RuntimeWaitFamily,
    cause: Option<u64>,
    awaited: bool,
    machine: Option<MachinePath>,
}

impl RuntimeWait {
    pub fn work(&self) -> &RuntimeWork {
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

    /// The machine the waiting task runs on, which a host's options name
    /// (`quiescence-is-analyzed`): the child whose queue started it, or its
    /// own anonymous root, reachable only through the analysis. Work that
    /// is not a reflection task runs on no machine.
    pub fn machine(&self) -> Option<&MachinePath> {
        self.machine.as_ref()
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

/// Input a machine left unread: items in one of its input buffers, or, in
/// a child's task queue, tasks that have not started. Work that did not
/// happen, whether or not anything was owed (maintainer, 2026-10-25).
#[derive(Clone, Debug)]
pub struct RuntimeBacklog {
    machine: MachinePath,
    port: Option<Key>,
    count: usize,
}

impl RuntimeBacklog {
    pub fn machine(&self) -> &MachinePath {
        &self.machine
    }

    /// The input port, or `None` for a child's task queue.
    pub fn port(&self) -> Option<&Key> {
        self.port.as_ref()
    }

    /// How many items are unread, or tasks have not started.
    pub fn count(&self) -> usize {
        self.count
    }
}

/// A family of unresolved items. By default each fails a batch; a client
/// may mask families, or recognize issues of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeIssueFamily {
    PromiseWait,
    ErredWait,
    Backlog,
    PendingObligation,
    MachineInError,
    DeliveryFailure,
    MaintenanceFailure,
}

/// One unresolved item of an analysis.
#[derive(Clone, Copy, Debug)]
pub enum RuntimeIssue<'a> {
    Wait(&'a RuntimeWait),
    Backlog(&'a RuntimeBacklog),
    Obligation(&'a PendingObligation),
    Machine(&'a FailedMachine),
    /// Output a machine sent to a host's endpoint, whose delivery failed,
    /// retained until the host acknowledges it.
    Delivery(&'a RuntimeDeliveryFailure),
    /// A collection the runtime ran for maintenance that failed, retained.
    Maintenance(&'a RuntimeMaintenanceFailure),
}

impl RuntimeIssue<'_> {
    pub fn family(&self) -> RuntimeIssueFamily {
        match self {
            Self::Backlog(_) => RuntimeIssueFamily::Backlog,
            Self::Obligation(_) => RuntimeIssueFamily::PendingObligation,
            Self::Machine(_) => RuntimeIssueFamily::MachineInError,
            Self::Delivery(_) => RuntimeIssueFamily::DeliveryFailure,
            Self::Maintenance(_) => RuntimeIssueFamily::MaintenanceFailure,
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
    busy_turns: Vec<MachinePath>,
    turn_version: u64,
    waits: Vec<RuntimeWait>,
    backlogs: Vec<RuntimeBacklog>,
    obligations: Vec<PendingObligation>,
    failed_machines: Vec<FailedMachine>,
    delivery_failures: Vec<Arc<RuntimeDeliveryFailure>>,
    maintenance_failures: Vec<RuntimeMaintenanceFailure>,
}

impl RuntimeAnalysis {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    /// Whether no work runs or can without action from outside, a turn's
    /// among it, but the asking thread's own.
    pub fn quiescent(&self) -> bool {
        self.quiescent
    }

    /// The machines where a turn is in progress, on any thread but the one
    /// that asked, whose turn is the observer: one that runs, or waits
    /// where what it observed has changed since. A turn waiting where what
    /// it observed holds is idle, and one joining its forks defers to them.
    pub fn busy_turns(&self) -> &[MachinePath] {
        &self.busy_turns
    }

    /// The version of the turns' runs that this analysis read, to wait on
    /// with [`EvaluationRuntime::wait_for_turns`].
    pub fn turn_version(&self) -> u64 {
        self.turn_version
    }

    /// Each unfinished work, by its id.
    pub fn waits(&self) -> &[RuntimeWait] {
        &self.waits
    }

    /// Each machine's input left unread, by machine and port.
    pub fn backlogs(&self) -> &[RuntimeBacklog] {
        &self.backlogs
    }

    /// The deliveries to hosts' endpoints that failed, until acknowledged.
    pub fn delivery_failures(&self) -> &[Arc<RuntimeDeliveryFailure>] {
        &self.delivery_failures
    }

    /// The maintenance collections that failed.
    pub fn maintenance_failures(&self) -> &[RuntimeMaintenanceFailure] {
        &self.maintenance_failures
    }

    /// The promises machines accepted that are still pending, by promise.
    pub fn obligations(&self) -> &[PendingObligation] {
        &self.obligations
    }

    /// The machines in error, flagged or failed anonymous roots, by path.
    pub fn failed_machines(&self) -> &[FailedMachine] {
        &self.failed_machines
    }

    /// What is unresolved: the waits that owe something, the backlogs, the
    /// pending obligations, the machines in error, then the retained
    /// delivery and maintenance failures. A promise wait counts once, at the head of its chain, which
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
            .chain(self.backlogs.iter().map(RuntimeIssue::Backlog))
            .chain(self.obligations.iter().map(RuntimeIssue::Obligation))
            .chain(self.failed_machines.iter().map(RuntimeIssue::Machine))
            .chain(
                self.delivery_failures
                    .iter()
                    .map(|failure| RuntimeIssue::Delivery(failure)),
            )
            .chain(
                self.maintenance_failures
                    .iter()
                    .map(RuntimeIssue::Maintenance),
            )
            .collect()
    }
}

impl EvaluationRuntime {
    /// Waits until a turn's run enters, leaves or changes what it does
    /// since an analysis that read `version` ([`RuntimeAnalysis::turn_version`]),
    /// or `timeout` passes: for a host whose analysis found turns busy.
    pub fn wait_for_turns(&self, version: u64, timeout: std::time::Duration) {
        self.state.shared_resources.wait_for_turns(version, timeout);
    }

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
        let (turn_version, busy_turns) = self
            .state
            .shared_resources
            .turns_in_progress(std::thread::current().id());
        let failed_roots = self.state.work.failed_roots();
        let (delivering, mut failures, accepted, backlogs, delivery_failures) = {
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
                state.events.backlogs(),
                state.events.outputs.failures.clone(),
            )
        };
        let maintenance_failures = self
            .state
            .shared_resources
            .mutation_admission
            .retained_gc_failures()
            .into_iter()
            .map(maintenance_failure)
            .collect();
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
            quiescent: coordinator.quiescent && !delivering && busy_turns.is_empty(),
            busy_turns: busy_turns.into_iter().map(MachinePath::from).collect(),
            turn_version,
            waits: coordinator
                .waits
                .into_iter()
                .map(|wait| RuntimeWait {
                    work: runtime_work_from_snapshot(core, wait.work),
                    family: match wait.family {
                        RuntimeWaitFamilySnapshot::Idle => RuntimeWaitFamily::Idle,
                        RuntimeWaitFamilySnapshot::InProgress => RuntimeWaitFamily::InProgress,
                        RuntimeWaitFamilySnapshot::PromiseWait => RuntimeWaitFamily::PromiseWait,
                        RuntimeWaitFamilySnapshot::ErredWait => RuntimeWaitFamily::ErredWait,
                    },
                    cause: wait.cause.map(|work| work.get()),
                    awaited: wait.awaited,
                    machine: wait.machine.map(|machine| match machine {
                        WaitMachineSnapshot::Child(child) => MachinePath::from(child.to_vec()),
                        WaitMachineSnapshot::AnonymousRoot(task) => {
                            MachinePath::from(crate::reflection::anonymous_root(task))
                        }
                    }),
                })
                .collect(),
            backlogs: backlogs
                .into_iter()
                .map(|(machine, port, count)| RuntimeBacklog {
                    machine: MachinePath::from(machine),
                    port,
                    count,
                })
                .collect(),
            delivery_failures: delivery_failures
                .iter()
                .map(|(_, failure)| failure.clone())
                .collect(),
            maintenance_failures,
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
