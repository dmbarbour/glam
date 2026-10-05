use super::*;

/// The recorded link of one socket in a [`RewriteBoundary`], typed by the
/// peer's sign as it was stored.
#[derive(Clone, Copy)]
enum BoundaryLink {
    /// A surviving port outside the rewritten pair.
    Port(SignedPort),
    /// Another socket of the same boundary: the pair linked to itself. The
    /// sign is that socket's.
    Socket(usize, Sign),
}

impl BoundaryLink {
    /// The sign of the socket's own port: the opposite of its peer's.
    fn socket_sign(self) -> Sign {
        match self {
            Self::Port(peer) => peer.sign.opposite(),
            Self::Socket(_, sign) => sign.opposite(),
        }
    }
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

/// One end of a resolved boundary path. A replacement port takes the sign of
/// the socket it replaces, so every rule inherits its typing from the pair.
#[derive(Clone, Copy)]
enum BoundaryEndpoint {
    Port(SignedPort),
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

    /// The sign the detached socket's own port had. A rule's new port that
    /// stands in for this socket takes this sign.
    pub(in crate::interaction_net::runtime) fn socket_sign(&self, socket: usize) -> Sign {
        self.links[socket].socket_sign()
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
                    .reference(port)
                    .expect("interaction auxiliary port must be wired");
                match ports.iter().position(|&socket| socket == neighbor.port) {
                    Some(socket) => BoundaryLink::Socket(socket, neighbor.sign),
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
            BoundaryLink::Port(peer) => Ok(BoundaryEndpoint::Port(peer)),
            BoundaryLink::Socket(other, _) => Err(other),
        };
        let replaced = |socket: usize| match replacements[socket] {
            BoundaryReplacement::Port(port) => Ok(BoundaryEndpoint::Port(SignedPort::new(
                port,
                boundary.socket_sign(socket),
            ))),
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
                self.wire(first, last);
            }
            (BoundaryEndpoint::Port(port), BoundaryEndpoint::Erase)
            | (BoundaryEndpoint::Erase, BoundaryEndpoint::Port(port)) => {
                // An eraser has no sign of its own: it takes the opposite of
                // whatever it meets.
                let erase = self.add_node(RuntimeNode::Erase);
                self.bind_reference(Port::principal(erase), port);
            }
            (BoundaryEndpoint::Erase, BoundaryEndpoint::Erase) => {}
        }
    }

    /// Anchors a providing port, such as a net's exposed port. The anchor
    /// consumes it.
    pub(in crate::interaction_net::runtime) fn add_interface(&mut self, target: Port) -> Port {
        let interface = self.add_node(RuntimeNode::Interface);
        let port = Port::auxiliary(interface, 1);
        self.connect_provider(target, port);
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
        self.reference(port).map(|peer| peer.port)
    }

    /// The typed reference stored at `port`: its peer and the peer's sign.
    pub(in crate::interaction_net::runtime) fn reference(&self, port: Port) -> Option<SignedPort> {
        let entry = self.nodes.get(&port.node())?;
        if port.index() >= entry.node.port_count() {
            return None;
        }
        entry.links[port.index() as usize].map(Link::peer)
    }

    /// `port` with its own sign, the opposite of its stored reference's.
    pub(in crate::interaction_net::runtime) fn signed(&self, port: Port) -> Option<SignedPort> {
        self.reference(port)
            .map(|peer| SignedPort::new(port, peer.sign.opposite()))
    }

    /// Disconnects `port` and returns the typed reference it held.
    pub(in crate::interaction_net::runtime) fn take_reference(
        &mut self,
        port: Port,
    ) -> Option<SignedPort> {
        let peer = self.reference(port)?;
        self.disconnect(port);
        Some(peer)
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

    /// Wires a providing port to a consuming one.
    pub(in crate::interaction_net::runtime) fn connect_provider(
        &mut self,
        provider: Port,
        consumer: Port,
    ) {
        self.wire(
            SignedPort::new(provider, Sign::Provides),
            SignedPort::new(consumer, Sign::Consumes),
        );
    }

    /// Binds an unwired port to a typed reference. The port takes the
    /// opposite sign, so a new node binds to existing structure without any
    /// rule assigning its signs.
    pub(in crate::interaction_net::runtime) fn bind_reference(
        &mut self,
        port: Port,
        reference: SignedPort,
    ) {
        self.wire(SignedPort::new(port, reference.sign.opposite()), reference);
    }

    /// Wires two ports of unknown polarity, for a hand-built test net. The
    /// net stops checking polarity, because its signs are arbitrary.
    #[cfg(test)]
    pub(in crate::interaction_net::runtime) fn connect(&mut self, left: Port, right: Port) {
        self.polarity_checked = false;
        self.connect_provider(left, right);
    }

    /// Writes one wire: each port's slot references the other, typed by the
    /// other's sign. Debug builds check that the wire joins opposite signs.
    pub(in crate::interaction_net::runtime) fn wire(
        &mut self,
        left: SignedPort,
        right: SignedPort,
    ) {
        debug_assert!(
            !self.polarity_checked() || left.sign != right.sign,
            "interaction-net rewrite joined two ports of the same sign: {left:?} and {right:?}"
        );
        let (left_sign, right_sign) = (left.sign, right.sign);
        let (left, right) = (left.port, right.port);
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
        self.nodes.get_mut(&left.node()).unwrap().links[left.index() as usize] =
            Some(Link::to(SignedPort::new(right, right_sign)));
        self.nodes.get_mut(&right.node()).unwrap().links[right.index() as usize] =
            Some(Link::to(SignedPort::new(left, left_sign)));
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

    /// Whether this net checks its polarity type. Release builds never do;
    /// a hand-built or deliberately unpolarized test net opts out.
    pub(in crate::interaction_net::runtime) fn polarity_checked(&self) -> bool {
        #[cfg(test)]
        {
            cfg!(debug_assertions) && self.polarity_checked
        }
        #[cfg(not(test))]
        {
            cfg!(debug_assertions)
        }
    }

    /// Checks one node's rule against the references its wired ports hold.
    /// A rule's created nodes must satisfy their own typing once wired.
    pub(in crate::interaction_net::runtime) fn debug_check_node_polarity(&self, node: NodeId) {
        if !self.polarity_checked() {
            return;
        }
        let Some(entry) = self.nodes.get(&node) else {
            return;
        };
        let peer = |index: usize| entry.links[index].map(|link| link.peer().sign);
        let expect = |index: usize, sign: Sign, what: &str| {
            if let Some(actual) = peer(index) {
                assert_eq!(
                    actual,
                    sign,
                    "interaction-net polarity violated: {what} of node {} must reference a {sign:?} port",
                    node.get()
                );
            }
        };
        match &entry.node {
            RuntimeNode::Data(_) => expect(0, Sign::Consumes, "data"),
            RuntimeNode::Operator(_) => {
                expect(0, Sign::Provides, "an operator's input");
                expect(1, Sign::Consumes, "an operator's result");
            }
            RuntimeNode::Bind => {
                expect(1, Sign::Provides, "a bind's first auxiliary");
                expect(2, Sign::Consumes, "a bind's second auxiliary");
            }
            RuntimeNode::Fan { .. } => {
                if let (Some(left), Some(right)) = (peer(1), peer(2)) {
                    assert_eq!(left, right, "a fan's branches must share one sign");
                }
                if let (Some(principal), Some(branch)) = (peer(0), peer(1).or(peer(2))) {
                    assert_ne!(
                        principal, branch,
                        "a fan's principal must oppose its branches"
                    );
                }
            }
            RuntimeNode::Interface => expect(1, Sign::Provides, "an interface anchor"),
            RuntimeNode::Erase
            | RuntimeNode::CallableCheckpoint(_)
            | RuntimeNode::RemoteCursor { .. } => {}
        }
    }

    /// Checks every node a rule created, once the rule has wired them.
    pub(in crate::interaction_net::runtime) fn debug_check_created_polarity(
        &self,
        nodes: &[NodeId],
    ) {
        for &node in nodes {
            self.debug_check_node_polarity(node);
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
