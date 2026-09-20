//! Lazy interpretation of interaction-net construction effects.

mod construction;
mod netlist;

#[cfg(test)]
mod netlist_inventory;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use construction::assert_construction_port_family_shape;
pub(in crate::eval) use construction::{NetConstructionMachine, NetConstructionPoll};
pub(in crate::eval) use netlist::interaction_net_from_netlist_in;
