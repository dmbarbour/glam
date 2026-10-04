use super::*;

/// The recorded link of one socket in a [`RewriteBoundary`].
#[derive(Clone, Copy)]
enum BoundaryLink {
    /// A surviving port outside the rewritten pair.
    Port(Port),
    /// Another socket of the same boundary: the pair linked to itself.
    Socket(usize),
}

/// The auxiliary ports of an active pair being rewritten, detached together.
///
/// Each port becomes a socket. Every neighbor is read before any link is
/// cleared, so a rule can resolve links between two ports of the pair itself.
pub(in crate::interaction_net::runtime) struct RewriteBoundary {
    links: Vec<BoundaryLink>,
}

/// What a rule substitutes for one socket of its [`RewriteBoundary`].
#[derive(Clone, Copy)]
pub(in crate::interaction_net::runtime) enum BoundaryReplacement {
    /// A port of a node the rule created.
    Port(Port),
    /// The socket fuses directly with another socket of the same boundary.
    /// Fusion is symmetric.
    Socket(usize),
    /// An eraser, created only if the resolved path reaches a port.
    Erase,
}

/// One end of a resolved boundary path.
#[derive(Clone, Copy)]
enum BoundaryEndpoint {
    Port(Port),
    Erase,
}

/// Lists one node's auxiliary ports in index order.
pub(in crate::interaction_net::runtime) fn auxiliary_ports(
    node: NodeId,
    count: u32,
) -> impl Iterator<Item = Port> {
    (1..=count).map(move |index| Port::auxiliary(node, index))
}

impl RewriteBoundary {
    pub(in crate::interaction_net::runtime) fn len(&self) -> usize {
        self.links.len()
    }
}

impl<S: NetSpecialization> RuntimeNet<S> {
    /// Detaches the given auxiliary ports of an active pair whose principal
    /// ports are already disconnected.
    pub(in crate::interaction_net::runtime) fn detach_boundary(
        &mut self,
        ports: &[Port],
    ) -> RewriteBoundary {
        let links = ports
            .iter()
            .map(|&port| {
                let neighbor = self
                    .neighbor(port)
                    .expect("interaction auxiliary port must be wired");
                match ports.iter().position(|&socket| socket == neighbor) {
                    Some(socket) => BoundaryLink::Socket(socket),
                    None => BoundaryLink::Port(neighbor),
                }
            })
            .collect();
        for &port in ports {
            // The second port of a link between two sockets is already clear.
            self.disconnect(port);
        }
        RewriteBoundary { links }
    }

    /// Connects a rule's replacements across a detached boundary.
    ///
    /// Recorded links and replacements alternate along paths through the
    /// sockets. Each path joins its two ends: two ports are wired together, a
    /// port meeting an eraser receives a fresh eraser, and two erasers vanish.
    /// Sockets linked only to one another form closed loops, which vanish.
    pub(in crate::interaction_net::runtime) fn attach_boundary(
        &mut self,
        boundary: RewriteBoundary,
        replacements: &[BoundaryReplacement],
    ) {
        assert_eq!(boundary.len(), replacements.len());
        debug_assert!(replacements.iter().enumerate().all(
            |(socket, replacement)| match replacement {
                BoundaryReplacement::Socket(other) => matches!(
                    replacements[*other],
                    BoundaryReplacement::Socket(back) if back == socket
                ),
                BoundaryReplacement::Port(_) | BoundaryReplacement::Erase => true,
            }
        ));
        let recorded = |socket: usize| match boundary.links[socket] {
            BoundaryLink::Port(port) => Ok(BoundaryEndpoint::Port(port)),
            BoundaryLink::Socket(other) => Err(other),
        };
        let replaced = |socket: usize| match replacements[socket] {
            BoundaryReplacement::Port(port) => Ok(BoundaryEndpoint::Port(port)),
            BoundaryReplacement::Erase => Ok(BoundaryEndpoint::Erase),
            BoundaryReplacement::Socket(other) => Err(other),
        };
        let mut visited = vec![false; boundary.len()];
        for start in 0..boundary.len() {
            for enter_by_replacement in [false, true] {
                if visited[start] {
                    break;
                }
                let entry = if enter_by_replacement {
                    replaced(start)
                } else {
                    recorded(start)
                };
                let Ok(first) = entry else {
                    continue;
                };
                let mut socket = start;
                let mut leave_by_replacement = !enter_by_replacement;
                let last = loop {
                    visited[socket] = true;
                    let next = if leave_by_replacement {
                        replaced(socket)
                    } else {
                        recorded(socket)
                    };
                    match next {
                        Ok(endpoint) => break endpoint,
                        Err(other) => {
                            socket = other;
                            leave_by_replacement = !leave_by_replacement;
                        }
                    }
                };
                self.join_boundary_endpoints(first, last);
            }
        }
    }

    fn join_boundary_endpoints(&mut self, first: BoundaryEndpoint, last: BoundaryEndpoint) {
        match (first, last) {
            (BoundaryEndpoint::Port(first), BoundaryEndpoint::Port(last)) => {
                self.connect(first, last);
            }
            (BoundaryEndpoint::Port(port), BoundaryEndpoint::Erase)
            | (BoundaryEndpoint::Erase, BoundaryEndpoint::Port(port)) => {
                let erase = self.add_node(RuntimeNode::Erase);
                self.connect(Port::principal(erase), port);
            }
            (BoundaryEndpoint::Erase, BoundaryEndpoint::Erase) => {}
        }
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
        self.cursor_obligations.remove(&node);
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
        let transferred_cursor = if left.is_principal() && right.is_principal() {
            let left_owned = self.cursor_obligations.contains_key(&left.node());
            let right_owned = self.cursor_obligations.contains_key(&right.node());
            assert!(
                !(left_owned && right_owned),
                "one active pair cannot inherit two pairless cursor obligations"
            );
            let cursor = if left_owned {
                Some(left.node())
            } else if right_owned {
                Some(right.node())
            } else {
                None
            };
            cursor.map(|cursor| {
                let obligation = self
                    .cursor_obligations
                    .remove(&cursor)
                    .expect("detected pairless cursor obligation must remain installed");
                assert!(
                    !obligation.state.is_claimed(),
                    "an in-flight pairless cursor claim cannot change graph ownership"
                );
                (cursor, obligation.state)
            })
        } else {
            None
        };
        self.nodes.get_mut(&left.node()).unwrap().links[left.index() as usize] = Some(right);
        self.nodes.get_mut(&right.node()).unwrap().links[right.index() as usize] = Some(left);
        if left.is_principal() && right.is_principal() {
            let pair = ActivePairKey::new(left.node(), right.node());
            let transferred_state = transferred_cursor.map(|(cursor, state)| match state {
                PairlessCursorState::Ready => ActivePairState::Ready,
                PairlessCursorState::Blocked(dependency) => ActivePairState::BlockedCursor {
                    cursor,
                    blockage: CursorBlockage::Dependency(dependency),
                },
                PairlessCursorState::Stable => ActivePairState::BlockedCursor {
                    cursor,
                    blockage: CursorBlockage::Stable,
                },
                PairlessCursorState::Claimed => {
                    unreachable!("claimed pairless cursor transfer was rejected")
                }
            });
            match self.active.entry(pair) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(transferred_state.unwrap_or(ActivePairState::Ready));
                }
                std::collections::btree_map::Entry::Occupied(mut entry)
                    if entry.get().is_claimed() && transferred_state.is_none() =>
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
