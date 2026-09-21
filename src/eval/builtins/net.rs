//! Lazy interpretation of interaction-net construction effects.

mod builder;
mod construction;
mod identity;
mod netlist;
mod runner;

#[cfg(test)]
mod netlist_inventory;
#[cfg(test)]
mod tests;

pub(in crate::eval) use builder::RegionalBuilderBuiltinMachine;
pub(super) use builder::apply_builder_builtin_in;
#[cfg(test)]
pub(in crate::eval) use builder::{
    construction_journal_lengths_for_test, construction_state_and_ports_for_test,
    decode_outcome_for_test, initial_state_for_test,
};
pub(in crate::eval) use construction::{NetConstructionMachine, NetConstructionPoll};
#[cfg(test)]
pub(crate) use identity::assert_construction_port_family_shape;
pub(in crate::eval) use netlist::interaction_net_from_netlist_in;
pub(in crate::eval) use runner::{RegionalBuilderEffectPoll, RegionalBuilderEffectRunner};
pub(in crate::eval) use runner::{RegionalNetConstruction, RegionalNetConstructionPoll};
