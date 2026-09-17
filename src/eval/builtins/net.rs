//! Lazy interpretation of interaction-net construction effects.

mod construction;

pub(in crate::eval) use construction::NetConstructionMachine;
#[cfg(test)]
pub(crate) use construction::assert_construction_port_family_shape;
