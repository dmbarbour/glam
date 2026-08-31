//! Generic interaction-net construction and reduction.
//!
//! The public surface is intentionally small: `model` defines the topology,
//! `builder` validates reusable templates, and `runtime` owns mutable reduction
//! state. Runtime implementation details remain below the runtime module.

mod builder;
mod model;
mod runtime;

pub(crate) use builder::{NetBuildError, NetBuilder};
pub(crate) use model::{
    ActivePairKey, InteractionNet, NetSpecialization, NodeId, OperatorYield, Port,
};
pub(crate) use runtime::{
    ActivePairStep, BlockedCall, BlockedOperatorCall, Call, CursorDependency,
    CursorDependencyDisposition, CursorDependencyResolution, CursorProgress, CursorStep,
    DemandEndpoint, FrontierObservation, InterfaceDemand, NetContention, OperatorCall,
    PreparedCopySource, Reduction, ReductionKind, RuntimeNet, RuntimeNetMutation,
    RuntimeNetRevisions, SharedRuntimeNet, StuckReason,
};

#[cfg(test)]
pub(crate) use model::Node;
