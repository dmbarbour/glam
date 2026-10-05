use super::graph::{BoundaryReplacement, auxiliary_ports};
use super::*;

// Every rule detaches all auxiliary ports of its active pair before creating
// replacements. A pair may be linked to itself, for example a Bind whose
// argument is its result; the boundary resolves such links instead of
// assuming each auxiliary neighbor survives the rewrite. Rules allocate their
// replacement nodes in a fixed order, which the payload edge-transition
// predictions rely on.
/// How an annihilation joins the two nodes' auxiliary ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::interaction_net::runtime) enum AuxiliaryPairing {
    /// Port `i` meets port `i`, as for identical fans.
    Positional,
    /// Port `1` meets port `2` and port `2` meets port `1`, as for binds.
    /// A bind's auxiliaries have opposite polarity: an application lists
    /// `[argument, result]` while a function lists `[result, argument]`, so
    /// each meets its counterpart. This makes polarity locally analyzable.
    Crossed,
}

impl<S: NetSpecialization> RuntimeNet<S> {
    pub(in crate::interaction_net::runtime) fn join(
        &mut self,
        left: NodeId,
        right: NodeId,
        auxiliaries: u32,
        pairing: AuxiliaryPairing,
    ) {
        self.disconnect(Port::principal(left));
        let ports = auxiliary_ports(left, auxiliaries)
            .chain(auxiliary_ports(right, auxiliaries))
            .collect::<Vec<_>>();
        let boundary = self.detach_boundary(&ports);
        self.remove_node(left);
        self.remove_node(right);
        // The boundary lists the left auxiliaries, then the right ones.
        let sockets = boundary.len();
        let replacements = (0..sockets)
            .map(|socket| {
                BoundaryReplacement::Socket(match pairing {
                    AuxiliaryPairing::Positional => (socket + sockets / 2) % sockets,
                    AuxiliaryPairing::Crossed => sockets - 1 - socket,
                })
            })
            .collect::<Vec<_>>();
        self.attach_boundary(boundary, &replacements);
    }

    pub(in crate::interaction_net::runtime) fn duplicate_data(
        &mut self,
        duplicator: &impl RuntimeNetPayloadDuplicator<S>,
        fan: NodeId,
        data: NodeId,
    ) {
        self.disconnect(Port::principal(fan));
        let boundary = self.detach_boundary(&auxiliary_ports(fan, 2).collect::<Vec<_>>());
        let RuntimeNode::Data(payload) = self.remove_node(data) else {
            unreachable!();
        };
        self.remove_node(fan);
        let clones = (0..boundary.len())
            .map(|_| self.add_node(RuntimeNode::Data(duplicator.duplicate_data(&payload))))
            .collect::<Vec<_>>();
        let replacements = clones
            .iter()
            .map(|clone| BoundaryReplacement::Port(Port::principal(*clone)))
            .collect::<Vec<_>>();
        self.attach_boundary(boundary, &replacements);
        self.debug_check_created_polarity(&clones);
    }

    pub(in crate::interaction_net::runtime) fn duplicate_bind(
        &mut self,
        fan: NodeId,
        identity: &FanIdentity,
        bind: NodeId,
    ) {
        self.disconnect(Port::principal(fan));
        let ports = auxiliary_ports(fan, 2)
            .chain(auxiliary_ports(bind, 2))
            .collect::<Vec<_>>();
        let boundary = self.detach_boundary(&ports);
        self.remove_node(fan);
        self.remove_node(bind);

        let binds = (0..2)
            .map(|_| self.add_node(RuntimeNode::Bind))
            .collect::<Vec<_>>();
        let residuals = (0..2)
            .map(|auxiliary| {
                let residual = self.add_node(RuntimeNode::Fan {
                    identity: identity.clone(),
                });
                // Each copied bind auxiliary keeps the original's sign; the
                // boundary lists the fan's two sockets, then the bind's.
                let sign = boundary.socket_sign(2 + auxiliary as usize);
                for (branch, bind) in binds.iter().enumerate() {
                    self.wire(
                        SignedPort::new(Port::auxiliary(*bind, auxiliary + 1), sign),
                        SignedPort::new(
                            Port::auxiliary(residual, branch as u32 + 1),
                            sign.opposite(),
                        ),
                    );
                }
                residual
            })
            .collect::<Vec<_>>();
        let replacements = binds
            .iter()
            .chain(&residuals)
            .map(|node| BoundaryReplacement::Port(Port::principal(*node)))
            .collect::<Vec<_>>();
        self.attach_boundary(boundary, &replacements);
        self.debug_check_created_polarity(&[binds, residuals].concat());
    }

    pub(in crate::interaction_net::runtime) fn duplicate_operator(
        &mut self,
        duplicator: &impl RuntimeNetPayloadDuplicator<S>,
        fan: NodeId,
        identity: &FanIdentity,
        operator: NodeId,
    ) {
        self.disconnect(Port::principal(fan));
        let ports = auxiliary_ports(fan, 2)
            .chain(auxiliary_ports(operator, 1))
            .collect::<Vec<_>>();
        let boundary = self.detach_boundary(&ports);
        let RuntimeNode::Operator(operator) = self.remove_node(operator) else {
            unreachable!();
        };
        self.remove_node(fan);

        let operators = (0..2)
            .map(|_| {
                self.add_node(RuntimeNode::Operator(
                    duplicator.duplicate_operator(&operator),
                ))
            })
            .collect::<Vec<_>>();
        let residual = self.add_node(RuntimeNode::Fan {
            identity: identity.clone(),
        });
        // Each copied operator result keeps the original's sign; the boundary
        // lists the fan's two sockets, then the operator's.
        let sign = boundary.socket_sign(2);
        for (branch, operator) in operators.iter().enumerate() {
            self.wire(
                SignedPort::new(Port::auxiliary(*operator, 1), sign),
                SignedPort::new(
                    Port::auxiliary(residual, branch as u32 + 1),
                    sign.opposite(),
                ),
            );
        }
        let replacements = operators
            .iter()
            .chain([&residual])
            .map(|node| BoundaryReplacement::Port(Port::principal(*node)))
            .collect::<Vec<_>>();
        self.attach_boundary(boundary, &replacements);
        self.debug_check_created_polarity(&[operators, vec![residual]].concat());
    }

    pub(in crate::interaction_net::runtime) fn commute_fans(
        &mut self,
        left: NodeId,
        left_identity: &FanIdentity,
        right: NodeId,
        right_identity: &FanIdentity,
    ) {
        self.disconnect(Port::principal(left));
        let ports = auxiliary_ports(left, 2)
            .chain(auxiliary_ports(right, 2))
            .collect::<Vec<_>>();
        let boundary = self.detach_boundary(&ports);
        self.remove_node(left);
        self.remove_node(right);

        let right_fans = (0..2u8)
            .map(|branch| {
                self.add_node(RuntimeNode::Fan {
                    identity: right_identity.residual(left_identity, branch),
                })
            })
            .collect::<Vec<_>>();
        let left_fans = (0..2u8)
            .map(|branch| {
                self.add_node(RuntimeNode::Fan {
                    identity: left_identity.residual(right_identity, branch),
                })
            })
            .collect::<Vec<_>>();
        // Each copy keeps its original's signs. The boundary lists the left
        // fan's two sockets, then the right fan's, and both branches of one
        // fan share a sign.
        let right_branch_sign = boundary.socket_sign(2);
        for (left_branch, right_fan) in right_fans.iter().enumerate() {
            for (right_branch, left_fan) in left_fans.iter().enumerate() {
                self.wire(
                    SignedPort::new(
                        Port::auxiliary(*right_fan, right_branch as u32 + 1),
                        right_branch_sign,
                    ),
                    SignedPort::new(
                        Port::auxiliary(*left_fan, left_branch as u32 + 1),
                        right_branch_sign.opposite(),
                    ),
                );
            }
        }
        let replacements = right_fans
            .iter()
            .chain(&left_fans)
            .map(|node| BoundaryReplacement::Port(Port::principal(*node)))
            .collect::<Vec<_>>();
        self.attach_boundary(boundary, &replacements);
        self.debug_check_created_polarity(&[right_fans, left_fans].concat());
    }

    pub(in crate::interaction_net::runtime) fn erase(&mut self, eraser: NodeId, other: NodeId) {
        self.disconnect(Port::principal(eraser));
        let auxiliaries = match self.node(other).expect("erased node must exist") {
            RuntimeNode::Bind | RuntimeNode::Fan { .. } => 2,
            RuntimeNode::Operator(_) => 1,
            RuntimeNode::Erase | RuntimeNode::Data(_) => 0,
            RuntimeNode::Interface
            | RuntimeNode::CallableCheckpoint(_)
            | RuntimeNode::RemoteCursor { .. } => {
                unreachable!("evaluator-only nodes are not erased as ordinary agents")
            }
        };
        let boundary =
            self.detach_boundary(&auxiliary_ports(other, auxiliaries).collect::<Vec<_>>());
        self.remove_node(eraser);
        self.remove_node(other);
        let replacements = vec![BoundaryReplacement::Erase; boundary.len()];
        self.attach_boundary(boundary, &replacements);
    }
}
