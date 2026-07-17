//! Generic interaction-net construction and reduction.
//!
//! The public surface is intentionally small: `model` defines the topology,
//! `builder` validates reusable templates, and `runtime` owns mutable reduction
//! state. Runtime implementation details remain below the runtime module.

mod builder;
mod model;
mod runtime;

#[allow(unused_imports)]
pub use builder::{BindSpine, CopyPorts, NetBuildError, NetBuilder};
#[allow(unused_imports)]
pub use model::{
    ActivePairKey, Callable, CopyId, DuplicationStep, FanIdentity, FanSite, InteractionNet,
    NetSpecialization, Node, NodeId, OperatorYield, Port, RuntimeNode, Wire,
};
#[allow(unused_imports)]
pub use runtime::{
    BlockedCall, BlockedCursor, Call, CursorDependency, CursorProgress, OperatorCall, Reduction,
    ReductionKind, RuntimeNet, SharedRuntimeNet, StuckPair, StuckReason,
};
