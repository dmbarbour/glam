use super::*;

impl<S: NetSpecialization> RuntimeNet<S> {
    pub(in crate::interaction_net::runtime) fn take_auxiliaries(
        &mut self,
        node: NodeId,
        count: u32,
    ) -> Vec<Port> {
        (1..=count)
            .map(|index| {
                self.disconnect(Port::auxiliary(node, index))
                    .expect("interaction auxiliary port must be wired")
            })
            .collect()
    }

    pub(in crate::interaction_net::runtime) fn add_interface(&mut self, target: Port) -> Port {
        let interface = self.add_node(RuntimeNode::Interface);
        let port = Port::auxiliary(interface, 1);
        self.connect(port, target);
        port
    }

    pub(in crate::interaction_net::runtime) fn assert_interface(&self, interface: Port) {
        assert_eq!(interface.index(), 1, "interface must use its boundary port");
        assert!(matches!(
            self.node(interface.node()),
            Some(RuntimeNode::Interface)
        ));
    }

    pub(in crate::interaction_net::runtime) fn add_node(&mut self, node: RuntimeNode<S>) -> NodeId {
        let id = NodeId::from_zero_based(self.next_node_id);
        self.next_node_id = self
            .next_node_id
            .checked_add(1)
            .expect("interaction-net node ID space exhausted");
        assert!(self.nodes.insert(id, RuntimeEntry::new(node)).is_none());
        id
    }

    pub(in crate::interaction_net::runtime) fn remove_node(
        &mut self,
        node: NodeId,
    ) -> RuntimeNode<S> {
        self.cursor_dependencies.remove(&node);
        let entry = self.nodes.remove(&node).expect("removed node must exist");
        assert!(entry.links.iter().all(Option::is_none));
        entry.node
    }

    pub(in crate::interaction_net::runtime) fn unschedule_node(&mut self, node: NodeId) {
        let Some(pair) = self.active_pair_key(node) else {
            return;
        };
        self.active.remove(&pair);
    }

    pub(in crate::interaction_net::runtime) fn neighbor(&self, port: Port) -> Option<Port> {
        let entry = self.nodes.get(&port.node())?;
        if port.index() >= entry.node.port_count() {
            return None;
        }
        entry.links[port.index() as usize]
    }

    pub(in crate::interaction_net::runtime) fn disconnect(&mut self, port: Port) -> Option<Port> {
        let neighbor = self.neighbor(port)?;
        self.nodes
            .get_mut(&port.node())
            .expect("disconnected node must exist")
            .links[port.index() as usize] = None;
        self.nodes
            .get_mut(&neighbor.node())
            .expect("neighbor node must exist")
            .links[neighbor.index() as usize] = None;
        Some(neighbor)
    }

    pub(in crate::interaction_net::runtime) fn connect(&mut self, left: Port, right: Port) {
        assert_ne!(left, right, "an interaction-net port cannot wire to itself");
        assert!(self.valid_port(left) && self.valid_port(right));
        assert!(self.neighbor(left).is_none() && self.neighbor(right).is_none());
        self.nodes.get_mut(&left.node()).unwrap().links[left.index() as usize] = Some(right);
        self.nodes.get_mut(&right.node()).unwrap().links[right.index() as usize] = Some(left);
        if left.is_principal() && right.is_principal() {
            let pair = ActivePairKey::new(left.node(), right.node());
            match self.active.entry(pair) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(ActivePairState::Ready);
                }
                std::collections::btree_map::Entry::Occupied(mut entry)
                    if entry.get().is_claimed() =>
                {
                    *entry.get_mut() = ActivePairState::Ready;
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    panic!("active pair must be new")
                }
            }
        }
    }

    pub(in crate::interaction_net::runtime) fn valid_port(&self, port: Port) -> bool {
        self.nodes
            .get(&port.node())
            .is_some_and(|entry| port.index() < entry.node.port_count())
    }

    pub(in crate::interaction_net::runtime) fn active_pair_key(
        &self,
        node: NodeId,
    ) -> Option<ActivePairKey> {
        let neighbor = self.neighbor(Port::principal(node))?;
        neighbor
            .is_principal()
            .then(|| ActivePairKey::new(node, neighbor.node()))
    }

    pub(in crate::interaction_net::runtime) fn pair_nodes(
        &self,
        pair: ActivePairKey,
    ) -> Option<(NodeId, NodeId)> {
        let left = pair.node();
        let right = self.neighbor(Port::principal(left))?;
        if !right.is_principal() || left >= right.node() {
            return None;
        }
        Some((left, right.node()))
    }

    #[cfg(test)]
    pub(in crate::interaction_net::runtime) fn principals_connect(
        &self,
        pair: ActivePairKey,
    ) -> bool {
        self.pair_nodes(pair).is_some()
    }
}
