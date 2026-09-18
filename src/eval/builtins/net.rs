//! Lazy interpretation of interaction-net construction effects.

mod construction;

#[cfg(test)]
pub(crate) use construction::assert_construction_port_family_shape;
pub(in crate::eval) use construction::{NetConstructionMachine, NetConstructionPoll};
