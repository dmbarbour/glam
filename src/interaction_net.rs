//! Generic interaction-net construction and reduction.
//!
//! The public surface is intentionally small: `model` defines the topology,
//! `builder` validates reusable templates, and `runtime` owns mutable reduction
//! state. Runtime implementation details remain below the runtime module.

mod builder;
mod model;
#[cfg(feature = "interaction-net-profiling")]
pub(crate) mod profiling;
mod runtime;

pub(crate) use builder::{NetBuildError, NetBuilder};
#[cfg(all(test, feature = "interaction-net-profiling"))]
pub(crate) use model::FanIdentity;
pub(crate) use model::{
    ActivePairKey, InteractionNet, NetSpecialization, NodeId, OperatorYield, Port,
};
pub(crate) use runtime::{
    ActivePairStep, BlockedCall, BlockedOperatorCall, Call, CursorDependency,
    CursorDependencyDisposition, CursorDependencyResolution, CursorProgress, CursorStep,
    DemandEndpoint, FrontierObservation, InterfaceDemand, NetContention, OperatorCall,
    PreparedCopySource, Reduction, ReductionKind, RuntimeNet, RuntimeNetCell,
    RuntimeNetEdgeTransition, RuntimeNetMutation, RuntimeNetMutationGateway, RuntimeNetPayload,
    SourceFrontier, StuckReason,
};
#[cfg(test)]
pub(crate) use runtime::{RuntimeNetRevisions, SharedRuntimeNet};

#[cfg(test)]
pub(crate) use model::{Node, RuntimeNode};
