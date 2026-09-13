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
}

impl NetDriverCounts {
    pub fn total_work(self) -> u64 {
        self.work_items
    }
}

/// One point-in-time profiling snapshot for an evaluation runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InteractionNetProfileSnapshot {
    pub reductions: NetReductionCounts,
    pub driver: NetDriverCounts,
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
});

/// Runtime-owned counter sink. All updates are intentionally relaxed: the
/// runtime lifecycle supplies the synchronization used before a stable
/// snapshot, and diagnostic snapshots need only be internally race-safe.
#[derive(Default)]
pub(crate) struct InteractionNetProfile {
    reductions: AtomicNetReductionCounts,
    driver: AtomicNetDriverCounts,
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
}

impl InteractionNetProfile {
    pub(crate) fn snapshot(&self) -> InteractionNetProfileSnapshot {
        InteractionNetProfileSnapshot {
            reductions: self.reductions.snapshot(),
            driver: self.driver.snapshot(),
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
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}
