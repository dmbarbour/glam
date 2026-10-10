//! Generic interaction-net construction and reduction.
//!
//! The public surface is intentionally small: `model` defines the topology,
//! `builder` validates reusable templates, and `runtime` owns mutable reduction
//! state. Runtime implementation details remain below the runtime module.

mod builder;
mod model;
pub(crate) mod polarity;
#[cfg(any(test, feature = "glam-prof"))]
#[cfg_attr(
    all(test, not(feature = "glam-prof")),
    expect(
        dead_code,
        reason = "without the profiling feature, tests compile this module only for the coordinator snapshot types; its rewrite and driver counters stay unused"
    )
)]
pub(crate) mod profiling;
mod runtime;

pub(crate) use builder::{NetBuildError, NetBuilder};
#[cfg(all(test, feature = "glam-prof"))]
pub(crate) use model::FanIdentity;
pub(crate) use model::{
    ActivePairKey, CopyId, FanSite, InteractionNet, NetSpecialization, NodeId, OperatorYield, Port,
};
pub(crate) use runtime::{
    ActivePairStep, BlockedCallableCheckpoint, Call, CallableCheckpointCall, CheckpointBlockResult,
    ClaimKind, CursorDependency, CursorDependencyDisposition, CursorDependencyResolution,
    CursorProgress, CursorStep, DemandEndpoint, FrontierObservation, InterfaceDemand,
    InterfaceRoute, NetContention, OperatorCall, PreparedCopySource, Reduction, ReductionKind,
    RuntimeNet, RuntimeNetCell, RuntimeNetEdgeTransition, RuntimeNetMutation,
    RuntimeNetMutationGateway, RuntimeNetPayload, RuntimeNetPayloadDuplicator, SourceFrontier,
    StuckReason,
};
#[cfg(test)]
pub(crate) use runtime::{RuntimeNetRevisions, SharedRuntimeNet, WholeCopy};

#[cfg(test)]
pub(crate) use model::{Node, RuntimeNode};
