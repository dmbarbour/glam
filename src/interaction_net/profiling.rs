//! Compile-time interaction-net accounting.
//!
//! This module exists only in `interaction-net-profiling` builds. Keeping the
//! feature boundary outside the reduction methods makes ordinary builds pay
//! no observer lookup, branch, or atomic-operation cost.

use std::sync::atomic::{AtomicU64, Ordering};

/// Reduction-order-invariant counts of committed interaction-net rewrites.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetReductionCounts {
    pub bind_join: u64,
    pub fan_join: u64,
    pub fan_commute: u64,
    pub fan_data: u64,
    pub fan_bind: u64,
    pub fan_operator: u64,
    pub erase: u64,
    pub call: u64,
    pub operator_call: u64,
    pub cursor_materialized: u64,
    pub cursor_joined: u64,
}

impl NetReductionCounts {
    pub fn total(self) -> u64 {
        self.bind_join
            + self.fan_join
            + self.fan_commute
            + self.fan_data
            + self.fan_bind
            + self.fan_operator
            + self.erase
            + self.call
            + self.operator_call
            + self.cursor_materialized
            + self.cursor_joined
    }
}

/// Schedule-sensitive counts from the cursor-WHNF driver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetDriverCounts {
    pub machine_polls: u64,
    pub work_items: u64,
    pub interface_polls: u64,
    pub cursor_steps: u64,
    pub active_pair_steps: u64,
    pub cursor_dependencies: u64,
    pub blocked_retries: u64,
    pub contentions: u64,
    pub disturbances: u64,
    pub request_root_restarts: u64,
    pub callable_whnf_inline_transitions: u64,
    pub callable_checkpoint_installs: u64,
    pub callable_checkpoint_resumptions: u64,
    pub callable_checkpoint_replacements: u64,
    pub callable_checkpoint_dependency_blocks: u64,
    pub callable_checkpoint_dependency_retries: u64,
    pub callable_checkpoint_stale_admissions: u64,
    pub callable_checkpoint_terminalizations: u64,
}

impl NetDriverCounts {
    pub fn total_work(self) -> u64 {
        self.work_items
    }
}

/// One point-in-time profiling snapshot for an evaluation runtime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InteractionNetProfileSnapshot {
    pub reductions: NetReductionCounts,
    pub driver: NetDriverCounts,
    pub exact_routes: ExactRouteMutationProfileSnapshot,
    pub coordinator_notifications: CoordinatorNotificationProfileSnapshot,
}

/// Coordinator mutations observed between an exact-route claim and guarded
/// release.
///
/// The fields follow the factual W9C coordinator vocabulary. They are
/// profiling observations, not scheduler policy: an occurrence does not by
/// itself imply that the retained route became invalid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoordinatorMutationCounts {
    pub demand_session_registry: u64,
    pub executor_availability: u64,
    pub fresh_work_admission: u64,
    pub work_activation: u64,
    pub client_demand_admission: u64,
    pub task_promise_index_admission: u64,
    pub task_promise_index_retirement: u64,
    pub work_claim: u64,
    pub work_requeue: u64,
    pub work_release: u64,
    pub client_demand_release: u64,
    pub dependency_promotion: u64,
    pub dependency_wake: u64,
    pub observation_wake: u64,
    pub cancellation: u64,
    pub session_closure: u64,
    pub terminal_settlement: u64,
    pub work_retirement: u64,
    pub failure_ledger: u64,
    pub stage_settlement: u64,
}

impl CoordinatorMutationCounts {
    pub fn total(self) -> u64 {
        self.demand_session_registry
            + self.executor_availability
            + self.fresh_work_admission
            + self.work_activation
            + self.client_demand_admission
            + self.task_promise_index_admission
            + self.task_promise_index_retirement
            + self.work_claim
            + self.work_requeue
            + self.work_release
            + self.client_demand_release
            + self.dependency_promotion
            + self.dependency_wake
            + self.observation_wake
            + self.cancellation
            + self.session_closure
            + self.terminal_settlement
            + self.work_retirement
            + self.failure_ledger
            + self.stage_settlement
    }
}

/// Release dispositions attached to attributed exact-route poll windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExactRouteDispositionCounts {
    pub runnable: u64,
    pub busy: u64,
    pub blocked_with_producer: u64,
    pub blocked_without_producer: u64,
    pub parked: u64,
    pub terminal: u64,
}

/// Fixed-cost W9C attribution for poll-generation invalidations.
///
/// `mutation_set_histogram` is materialized only when a snapshot is requested.
/// Each key is a bitset in the declaration order of
/// [`CoordinatorMutationCounts`]; the runtime retains only aggregate counters,
/// never an event history or work identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExactRouteMutationProfileSnapshot {
    pub invalidated_windows: u64,
    pub mutation_occurrences: CoordinatorMutationCounts,
    pub synchronous_mutation_occurrences: CoordinatorMutationCounts,
    pub external_mutation_occurrences: CoordinatorMutationCounts,
    pub windows_containing: CoordinatorMutationCounts,
    pub mutation_set_histogram: Vec<(u32, u64)>,
    pub total_route_depth: u64,
    pub maximum_route_depth: u64,
    pub dispositions: ExactRouteDispositionCounts,
}

/// Calls made through the coordinator's shared work condition variable.
///
/// These counters describe notification policy, not semantic work. In
/// particular, a mutation may advance `work_generation` without notifying a
/// parked host thread when it cannot satisfy any parked predicate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoordinatorNotificationCallCounts {
    pub notify_one: CoordinatorMutationCounts,
    pub notify_all: CoordinatorMutationCounts,
}

/// The first bounded observation made by one waiter after it was released.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoordinatorWaiterOutcomeCounts {
    pub released: u64,
    pub productive: u64,
    pub relevant: u64,
    pub unrelated: u64,
}

/// Schedule-sensitive W9C accounting for the shared coordinator condition
/// variable.
///
/// The runtime retains aggregate counts only. It does not retain thread,
/// waiter, work, route, or notification identities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoordinatorNotificationProfileSnapshot {
    pub calls: CoordinatorNotificationCallCounts,
    pub workers: CoordinatorWaiterOutcomeCounts,
    pub exact_clients: CoordinatorWaiterOutcomeCounts,
    pub session_drains: CoordinatorWaiterOutcomeCounts,
    pub task_observers: CoordinatorWaiterOutcomeCounts,
}

macro_rules! atomic_counts {
    (
        $name:ident => $snapshot:ident {
            $($field:ident),+ $(,)?
        }
    ) => {
        #[derive(Default)]
        struct $name {
            $(pub(super) $field: AtomicU64),+
        }

        impl $name {
            fn snapshot(&self) -> $snapshot {
                $snapshot {
                    $($field: self.$field.load(Ordering::Relaxed)),+
                }
            }
        }
    };
}

atomic_counts!(AtomicNetReductionCounts => NetReductionCounts {
    bind_join,
    fan_join,
    fan_commute,
    fan_data,
    fan_bind,
    fan_operator,
    erase,
    call,
    operator_call,
    cursor_materialized,
    cursor_joined,
});

atomic_counts!(AtomicNetDriverCounts => NetDriverCounts {
    machine_polls,
    work_items,
    interface_polls,
    cursor_steps,
    active_pair_steps,
    cursor_dependencies,
    blocked_retries,
    contentions,
    disturbances,
    request_root_restarts,
    callable_whnf_inline_transitions,
    callable_checkpoint_installs,
    callable_checkpoint_resumptions,
    callable_checkpoint_replacements,
    callable_checkpoint_dependency_blocks,
    callable_checkpoint_dependency_retries,
    callable_checkpoint_stale_admissions,
    callable_checkpoint_terminalizations,
});

/// Runtime-owned counter sink. All updates are intentionally relaxed: the
/// runtime lifecycle supplies the synchronization used before a stable
/// snapshot, and diagnostic snapshots need only be internally race-safe.
#[derive(Default)]
pub(crate) struct InteractionNetProfile {
    reductions: AtomicNetReductionCounts,
    driver: AtomicNetDriverCounts,
    #[cfg(test)]
    driver_work_item_limit: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReductionEvent {
    BindJoin,
    FanJoin,
    FanCommute,
    FanData,
    FanBind,
    FanOperator,
    Erase,
    Call,
    OperatorCall,
    CursorMaterialized,
    CursorJoined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DriverEvent {
    MachinePoll,
    WorkItem,
    InterfacePoll,
    CursorStep,
    ActivePairStep,
    CursorDependency,
    BlockedRetry,
    Contention,
    Disturbance,
    RequestRootRestart,
    CallableWhnfInlineTransition,
    CallableCheckpointInstall,
    CallableCheckpointResumption,
    CallableCheckpointReplacement,
    CallableCheckpointDependencyBlock,
    CallableCheckpointDependencyRetry,
    CallableCheckpointStaleAdmission,
    CallableCheckpointTerminalization,
}

impl InteractionNetProfile {
    pub(crate) fn snapshot(&self) -> InteractionNetProfileSnapshot {
        InteractionNetProfileSnapshot {
            reductions: self.reductions.snapshot(),
            driver: self.driver.snapshot(),
            exact_routes: ExactRouteMutationProfileSnapshot::default(),
            coordinator_notifications: CoordinatorNotificationProfileSnapshot::default(),
        }
    }

    pub(crate) fn record_reduction(&self, event: ReductionEvent) {
        let counter = match event {
            ReductionEvent::BindJoin => &self.reductions.bind_join,
            ReductionEvent::FanJoin => &self.reductions.fan_join,
            ReductionEvent::FanCommute => &self.reductions.fan_commute,
            ReductionEvent::FanData => &self.reductions.fan_data,
            ReductionEvent::FanBind => &self.reductions.fan_bind,
            ReductionEvent::FanOperator => &self.reductions.fan_operator,
            ReductionEvent::Erase => &self.reductions.erase,
            ReductionEvent::Call => &self.reductions.call,
            ReductionEvent::OperatorCall => &self.reductions.operator_call,
            ReductionEvent::CursorMaterialized => &self.reductions.cursor_materialized,
            ReductionEvent::CursorJoined => &self.reductions.cursor_joined,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_driver(&self, event: DriverEvent) {
        self.record_driver_by(event, 1);
    }

    pub(crate) fn record_driver_by(&self, event: DriverEvent, count: u64) {
        let counter = match event {
            DriverEvent::MachinePoll => &self.driver.machine_polls,
            DriverEvent::WorkItem => &self.driver.work_items,
            DriverEvent::InterfacePoll => &self.driver.interface_polls,
            DriverEvent::CursorStep => &self.driver.cursor_steps,
            DriverEvent::ActivePairStep => &self.driver.active_pair_steps,
            DriverEvent::CursorDependency => &self.driver.cursor_dependencies,
            DriverEvent::BlockedRetry => &self.driver.blocked_retries,
            DriverEvent::Contention => &self.driver.contentions,
            DriverEvent::Disturbance => &self.driver.disturbances,
            DriverEvent::RequestRootRestart => &self.driver.request_root_restarts,
            DriverEvent::CallableWhnfInlineTransition => {
                &self.driver.callable_whnf_inline_transitions
            }
            DriverEvent::CallableCheckpointInstall => &self.driver.callable_checkpoint_installs,
            DriverEvent::CallableCheckpointResumption => {
                &self.driver.callable_checkpoint_resumptions
            }
            DriverEvent::CallableCheckpointReplacement => {
                &self.driver.callable_checkpoint_replacements
            }
            DriverEvent::CallableCheckpointDependencyBlock => {
                &self.driver.callable_checkpoint_dependency_blocks
            }
            DriverEvent::CallableCheckpointDependencyRetry => {
                &self.driver.callable_checkpoint_dependency_retries
            }
            DriverEvent::CallableCheckpointStaleAdmission => {
                &self.driver.callable_checkpoint_stale_admissions
            }
            DriverEvent::CallableCheckpointTerminalization => {
                &self.driver.callable_checkpoint_terminalizations
            }
        };
        counter.fetch_add(count, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn set_driver_work_item_limit(&self, limit: u64) {
        assert_ne!(limit, 0, "a profiling work-item limit must be positive");
        assert_eq!(
            self.driver.work_items.load(Ordering::Relaxed),
            0,
            "a profiling work-item limit must be installed before net work begins"
        );
        self.driver_work_item_limit.store(limit, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn driver_work_item_limit_reached(&self) -> bool {
        let limit = self.driver_work_item_limit.load(Ordering::Relaxed);
        limit != 0 && self.driver.work_items.load(Ordering::Relaxed) >= limit
    }
}
