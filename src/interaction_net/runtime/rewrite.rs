use super::*;

impl<S: NetSpecialization> RuntimeNet<S> {
    pub(in crate::interaction_net::runtime) fn join(
        &mut self,
        left: NodeId,
        right: NodeId,
        auxiliaries: u32,
    ) {
        self.disconnect(Port::principal(left));
        let left_neighbors = self.take_auxiliaries(left, auxiliaries);
        let right_neighbors = self.take_auxiliaries(right, auxiliaries);
        self.remove_node(left);
        self.remove_node(right);
        for (left, right) in left_neighbors.into_iter().zip(right_neighbors) {
            self.connect(left, right);
        }
    }

    pub(in crate::interaction_net::runtime) fn duplicate_data(
        &mut self,
        fan: NodeId,
        data: NodeId,
    ) {
        self.disconnect(Port::principal(fan));
        let targets = self.take_auxiliaries(fan, 2);
        let RuntimeNode::Data(payload) = self.remove_node(data) else {
            unreachable!();
        };
        self.remove_node(fan);
        for target in targets {
            let clone = self.add_node(RuntimeNode::Data(payload.clone()));
            self.connect(Port::principal(clone), target);
        }
    }

    pub(in crate::interaction_net::runtime) fn duplicate_bind(
        &mut self,
        fan: NodeId,
        identity: &FanIdentity,
        bind: NodeId,
    ) {
        self.disconnect(Port::principal(fan));
        let fan_targets = self.take_auxiliaries(fan, 2);
        let bind_targets = self.take_auxiliaries(bind, 2);
        self.remove_node(fan);
        self.remove_node(bind);

        let binds = fan_targets
            .into_iter()
            .map(|target| {
                let node = self.add_node(RuntimeNode::Bind);
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        for (auxiliary, target) in bind_targets.into_iter().enumerate() {
            let residual = self.add_node(RuntimeNode::Fan {
                identity: identity.clone(),
            });
            self.connect(Port::principal(residual), target);
            for (branch, bind) in binds.iter().enumerate() {
                self.connect(
                    Port::auxiliary(residual, branch as u32 + 1),
                    Port::auxiliary(*bind, auxiliary as u32 + 1),
                );
            }
        }
    }

    pub(in crate::interaction_net::runtime) fn duplicate_operator(
        &mut self,
        fan: NodeId,
        identity: &FanIdentity,
        operator: NodeId,
    ) {
        self.disconnect(Port::principal(fan));
        let fan_targets = self.take_auxiliaries(fan, 2);
        let [result] = <[Port; 1]>::try_from(self.take_auxiliaries(operator, 1)).unwrap();
        let RuntimeNode::Operator(operator) = self.remove_node(operator) else {
            unreachable!();
        };
        self.remove_node(fan);

        let operators = fan_targets
            .into_iter()
            .map(|target| {
                let node = self.add_node(RuntimeNode::Operator(operator.clone()));
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        let residual = self.add_node(RuntimeNode::Fan {
            identity: identity.clone(),
        });
        self.connect(Port::principal(residual), result);
        for (branch, operator) in operators.into_iter().enumerate() {
            self.connect(
                Port::auxiliary(residual, branch as u32 + 1),
                Port::auxiliary(operator, 1),
            );
        }
    }

    pub(in crate::interaction_net::runtime) fn commute_fans(
        &mut self,
        left: NodeId,
        left_identity: &FanIdentity,
        right: NodeId,
        right_identity: &FanIdentity,
    ) {
        self.disconnect(Port::principal(left));
        let left_targets = self.take_auxiliaries(left, 2);
        let right_targets = self.take_auxiliaries(right, 2);
        self.remove_node(left);
        self.remove_node(right);

        let right_fans = left_targets
            .into_iter()
            .enumerate()
            .map(|(branch, target)| {
                let node = self.add_node(RuntimeNode::Fan {
                    identity: right_identity.residual(left_identity, branch as u8),
                });
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        let left_fans = right_targets
            .into_iter()
            .enumerate()
            .map(|(branch, target)| {
                let node = self.add_node(RuntimeNode::Fan {
                    identity: left_identity.residual(right_identity, branch as u8),
                });
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        for (left_branch, right_fan) in right_fans.iter().enumerate() {
            for (right_branch, left_fan) in left_fans.iter().enumerate() {
                self.connect(
                    Port::auxiliary(*right_fan, right_branch as u32 + 1),
                    Port::auxiliary(*left_fan, left_branch as u32 + 1),
                );
            }
        }
    }

    pub(in crate::interaction_net::runtime) fn erase(&mut self, eraser: NodeId, other: NodeId) {
        self.disconnect(Port::principal(eraser));
        let auxiliaries = match self.node(other).expect("erased node must exist") {
            RuntimeNode::Bind | RuntimeNode::Fan { .. } => 2,
            RuntimeNode::Operator(_) => 1,
            RuntimeNode::Erase | RuntimeNode::Data(_) => 0,
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => {
                unreachable!("evaluator-only nodes are not erased as ordinary agents")
            }
        };
        let targets = self.take_auxiliaries(other, auxiliaries);
        self.remove_node(eraser);
        self.remove_node(other);
        for target in targets {
            let erase = self.add_node(RuntimeNode::Erase);
            self.connect(Port::principal(erase), target);
        }
    }
}
