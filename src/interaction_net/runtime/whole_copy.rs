//! Copies of source nets with no evaluation left, installed whole.
//!
//! A copy through a remote cursor shares whatever evaluation its source has
//! left, one node per cursor step. A source with no active pair, copy,
//! cursor obligation or callable checkpoint can never reduce again, so such
//! a copy shares nothing; this module captures the source under its own lock
//! and installs the copy in one step under the target's.

use super::*;

/// The most nodes a source may have and still be copied whole. A whole copy
/// works under the source's lock and copies parts its caller may never
/// demand; the sources measured average about five nodes and reach twenty
/// (`perf-reduced-source-copy`).
const WHOLE_COPY_MAX_NODES: usize = 64;

/// A source net with no evaluation left, captured for one copy: the nodes
/// reachable from its interface, each with its payload duplicated and the
/// links of its ports given as positions in `nodes`. The first node's port
/// `root` faced the interface, with sign `root_sign`.
pub(crate) struct WholeCopy<S: NetSpecialization> {
    nodes: Vec<WholeCopyNode<S>>,
    root: u32,
    root_sign: Sign,
}

struct WholeCopyNode<S: NetSpecialization> {
    node: RuntimeNode<S>,
    links: [Option<WholeCopyLink>; 3],
}

/// A link to port `port` of the node at position `node`, a port of sign
/// `sign`.
#[derive(Clone, Copy)]
struct WholeCopyLink {
    node: usize,
    port: u32,
    sign: Sign,
}

impl<S: NetSpecialization> WholeCopy<S> {
    /// How many nodes the copy installs.
    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }
}

impl<S: NetSpecialization> RuntimeNet<S> {
    /// Captures this net for a whole copy if no evaluation is left in it: no
    /// active pair, copy, cursor obligation or callable checkpoint, and at
    /// most [`WHOLE_COPY_MAX_NODES`] nodes reachable from its interface.
    pub(crate) fn whole_copy(
        &self,
        duplicator: &impl RuntimeNetPayloadDuplicator<S>,
    ) -> Option<WholeCopy<S>> {
        if !self.active.is_empty() || !self.copies.is_empty() || !self.cursor_obligations.is_empty()
        {
            return None;
        }
        let interface = self.exposed?;
        let root = self.signed(self.neighbor(interface)?)?;

        // The nodes reachable from the interface, in the order found.
        let mut order = vec![root.port.node()];
        let mut positions = TrustedHashMap::default();
        positions.insert(root.port.node(), 0);
        let mut next = 0;
        while let Some(&node) = order.get(next) {
            next += 1;
            let entry = self.node(node)?;
            if matches!(
                entry,
                RuntimeNode::Interface
                    | RuntimeNode::RemoteCursor { .. }
                    | RuntimeNode::CallableCheckpoint(_)
            ) {
                return None;
            }
            for index in 0..entry.port_count() {
                let Some(peer) = self.neighbor(Port::new(node, index)) else {
                    continue;
                };
                if peer == interface || positions.contains_key(&peer.node()) {
                    continue;
                }
                if order.len() == WHOLE_COPY_MAX_NODES {
                    return None;
                }
                positions.insert(peer.node(), order.len());
                order.push(peer.node());
            }
        }

        let nodes = order
            .iter()
            .map(|&node| {
                let source = self.node(node).expect("a reached node exists");
                let mut links = [None; 3];
                for index in 0..source.port_count() {
                    let Some(peer) = self.reference(Port::new(node, index)) else {
                        continue;
                    };
                    if peer.port == interface {
                        continue;
                    }
                    links[index as usize] = Some(WholeCopyLink {
                        node: positions[&peer.port.node()],
                        port: peer.port.index(),
                        sign: peer.sign,
                    });
                }
                WholeCopyNode {
                    node: source
                        .duplicate_copyable(duplicator)
                        .expect("a whole copy holds no callable checkpoint"),
                    links,
                }
            })
            .collect();
        Some(WholeCopy {
            nodes,
            root: root.port.index(),
            root_sign: root.sign,
        })
    }

    /// Installs a whole copy with its root bound to `reference`, wiring its
    /// nodes as their source was wired, with fresh fan sites, and returns
    /// the root node. Its nodes take the next node IDs in order, so the
    /// transition reports them as one run of fresh nodes.
    pub(in crate::interaction_net::runtime) fn install_whole_copy(
        &mut self,
        copy: WholeCopy<S>,
        reference: SignedPort,
    ) -> NodeId {
        debug_assert_eq!(
            copy.root_sign,
            reference.sign.opposite(),
            "a whole copy's root keeps its source sign"
        );
        let WholeCopy { nodes, root, .. } = copy;
        let mut fan_sites = TrustedHashMap::default();
        let mut links = Vec::with_capacity(nodes.len());
        let mut ids = Vec::with_capacity(nodes.len());
        for WholeCopyNode { node, links: ports } in nodes {
            let node = match node {
                RuntimeNode::Fan { identity } => RuntimeNode::Fan {
                    identity: self.translate_fan_identity(&mut fan_sites, &identity),
                },
                node => node,
            };
            ids.push(self.add_node(node));
            links.push(ports);
        }
        for (position, ports) in links.iter().enumerate() {
            for (index, link) in ports.iter().enumerate() {
                let index = index as u32;
                // Both ends record each wire; write it from the earlier one.
                let Some(link) = link.filter(|link| (position, index) < (link.node, link.port))
                else {
                    continue;
                };
                self.wire(
                    SignedPort::new(Port::new(ids[position], index), link.sign.opposite()),
                    SignedPort::new(Port::new(ids[link.node], link.port), link.sign),
                );
            }
        }
        self.bind_reference(Port::new(ids[0], root), reference);
        self.debug_check_created_polarity(&ids);
        ids[0]
    }
}
