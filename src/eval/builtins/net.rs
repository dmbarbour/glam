//! Lazy interpretation of interaction-net construction effects.

mod builder;
mod construction;
mod netlist;

#[cfg(test)]
mod netlist_inventory;
#[cfg(test)]
mod tests;

pub(in crate::eval) use builder::RegionalBuilderBuiltinMachine;
pub(super) use builder::apply_builder_builtin_in;
#[cfg(test)]
pub(crate) use construction::assert_construction_port_family_shape;
pub(in crate::eval) use construction::{NetConstructionMachine, NetConstructionPoll};
pub(in crate::eval) use netlist::interaction_net_from_netlist_in;
