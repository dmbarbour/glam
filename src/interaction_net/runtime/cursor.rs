use super::*;

impl<S: NetSpecialization> RuntimeNet<S> {
    /// Starts one logical copy and returns its initially unwired remote cursor.
    pub fn begin_copy(&mut self, source: SharedRuntimeNet<S>) -> NodeId {
        let remote = source.with(RuntimeNet::exposed);
        let copy = CopyId(self.next_copy_id);
        self.next_copy_id = self
            .next_copy_id
            .checked_add(1)
            .expect("interaction-net copy ID space exhausted");
        assert!(
            self.copies
                .insert(
                    copy,
                    CopyState {
                        source,
                        frontiers: HashMap::new(),
                        fan_sites: HashMap::new(),
                    },
                )
                .is_none()
        );
        let cursor = self.add_node(RuntimeNode::RemoteCursor { copy, remote });
        self.copies
            .get_mut(&copy)
            .unwrap()
            .frontiers
            .insert(remote, cursor);
        cursor
    }

    /// Completes claimed applicable lowering by loading the
    /// resulting closed net at the original application's principal port.
    pub(in crate::interaction_net::runtime) fn resume_claimed_call_with_copy(
        &mut self,
        call: Call,
        source: SharedRuntimeNet<S>,
    ) -> NodeId {
        self.attach_call_to_copy(call, source)
    }

    pub(in crate::interaction_net::runtime) fn attach_call_to_copy(
        &mut self,
        call: Call,
        source: SharedRuntimeNet<S>,
    ) -> NodeId {
        assert_eq!(
            self.active.remove(&call.pair),
            Some(ActivePairState::Claimed),
            "resumed interaction-net call must still be claimed"
        );
        assert_eq!(
            self.disconnect(Port::principal(call.bind)),
            Some(Port::principal(call.data))
        );
        assert!(matches!(self.remove_node(call.data), RuntimeNode::Data(_)));
        let cursor = self.begin_copy(source);
        self.connect(Port::principal(call.bind), Port::principal(cursor));
        cursor
    }

    /// Completes applicable lowering by replacing callable data with
    /// an explicit unary function net. The newly introduced Bind then joins
    /// the original application Bind through the ordinary interaction rule.
    pub(in crate::interaction_net::runtime) fn resume_claimed_call_with_operator(
        &mut self,
        call: Call,
        operator: S::Operator,
    ) -> NodeId {
        assert_eq!(
            self.active.remove(&call.pair),
            Some(ActivePairState::Claimed),
            "lowered operator call must still be claimed"
        );
        assert_eq!(
            self.disconnect(Port::principal(call.bind)),
            Some(Port::principal(call.data))
        );
        assert!(matches!(self.remove_node(call.data), RuntimeNode::Data(_)));

        let function = self.add_node(RuntimeNode::Bind);
        let operator = self.add_node(RuntimeNode::Operator(operator));
        self.connect(Port::principal(call.bind), Port::principal(function));
        self.connect(Port::auxiliary(function, 1), Port::principal(operator));
        self.connect(Port::auxiliary(function, 2), Port::auxiliary(operator, 1));
        function
    }

    /// Replaces an evaluator interface with one embedded data node.
    pub fn complete_interface_with_data(&mut self, interface: Port, data: S::Data) -> NodeId {
        self.assert_interface(interface);
        let target = self
            .disconnect(interface)
            .expect("completed interaction-net interface must remain wired");
        self.remove_node(interface.node());
        let node = self.add_node(RuntimeNode::Data(data));
        self.connect(Port::principal(node), target);
        node
    }

    pub(in crate::interaction_net::runtime) fn take_operator_call(
        &mut self,
        call: OperatorCall,
    ) -> Port {
        self.remove_pending_operator_call(call);
        assert_eq!(
            self.disconnect(Port::principal(call.operator)),
            Some(Port::principal(call.data))
        );
        let target = self
            .disconnect(Port::auxiliary(call.operator, 1))
            .expect("operator result must remain wired");
        assert!(matches!(
            self.remove_node(call.operator),
            RuntimeNode::Operator(_)
        ));
        assert!(matches!(self.remove_node(call.data), RuntimeNode::Data(_)));
        target
    }

    pub(in crate::interaction_net::runtime) fn remove_pending_operator_call(
        &mut self,
        call: OperatorCall,
    ) {
        assert_eq!(
            self.active.remove(&call.pair),
            Some(ActivePairState::Claimed),
            "completed operator call must still be pending"
        );
    }

    pub(in crate::interaction_net::runtime) fn begin_cursor_claim(
        &mut self,
        cursor: NodeId,
        expected_pair: Option<ActivePairKey>,
    ) -> Option<CursorProgress> {
        let pair = expected_pair.or_else(|| self.active_pair_key(cursor));
        if let Some(expected) = expected_pair {
            assert_eq!(pair, Some(expected));
        }
        if let Some(pair) = pair {
            match self.active.get_mut(&pair) {
                Some(state @ ActivePairState::Ready)
                | Some(state @ ActivePairState::BlockedCursor { .. }) => {
                    *state = ActivePairState::Claimed;
                }
                Some(ActivePairState::Claimed) if expected_pair == Some(pair) => {}
                _ => return None,
            }
        }
        self.cursor_dependencies.remove(&cursor);
        Some(CursorProgress::Claimed)
    }

    pub(in crate::interaction_net::runtime) fn cursor_claim(
        &self,
        cursor: NodeId,
    ) -> Option<CursorClaim<S>> {
        let pair = self.active_pair_key(cursor);
        if pair.is_some_and(|pair| self.active.get(&pair) != Some(&ActivePairState::Claimed)) {
            return None;
        }
        let RuntimeNode::RemoteCursor { copy, remote } = self.node(cursor)?.clone() else {
            return None;
        };
        let source = self.copies.get(&copy)?.source.clone();
        Some(CursorClaim {
            cursor,
            pair,
            copy,
            remote,
            source,
        })
    }

    pub(in crate::interaction_net::runtime) fn inspect_source_frontier(
        &self,
        remote: Port,
    ) -> SourceFrontier<S> {
        let port = self
            .neighbor(remote)
            .expect("remote cursor anchor must remain wired in its source");
        if port.is_principal() {
            let node = self
                .node(port.node())
                .expect("remote cursor neighbor must exist")
                .clone();
            return SourceFrontier::Principal { port, node };
        }

        let principal_neighbor = self.neighbor(Port::principal(port.node()));
        if let Some(partner) = principal_neighbor.filter(|neighbor| neighbor.is_principal()) {
            return SourceFrontier::ActiveAuxiliary {
                entered: port,
                partner,
            };
        }
        let mut principal_anchors = Vec::new();
        let mut terminal_pair = None;
        let mut node = port.node();
        let mut visited = HashSet::new();
        while visited.insert(node) {
            let Some(neighbor) = self.neighbor(Port::principal(node)) else {
                break;
            };
            if neighbor.is_principal() {
                terminal_pair = Some(ActivePairKey::new(node, neighbor.node()));
                break;
            }
            principal_anchors.push(neighbor);
            node = neighbor.node();
        }
        SourceFrontier::StableAuxiliary {
            port,
            principal_anchors,
            terminal_pair,
        }
    }

    pub(in crate::interaction_net::runtime) fn finish_cursor_claim(
        &mut self,
        claim: CursorClaim<S>,
        frontier: SourceFrontier<S>,
    ) -> CursorProgress {
        if let Some(pair) = claim.pair {
            assert_eq!(self.active.get(&pair), Some(&ActivePairState::Claimed));
        }
        assert!(matches!(
            self.node(claim.cursor),
            Some(RuntimeNode::RemoteCursor { copy, remote })
                if *copy == claim.copy && *remote == claim.remote
        ));
        let frontier_port = match &frontier {
            SourceFrontier::Principal { port, .. }
            | SourceFrontier::StableAuxiliary { port, .. } => *port,
            SourceFrontier::ActiveAuxiliary { entered, .. } => *entered,
        };

        let converging_cursor = self
            .copies
            .get(&claim.copy)
            .expect("claimed cursor must reference a live copy")
            .frontiers
            .get(&frontier_port)
            .copied();
        let progress = if let Some(peer) = converging_cursor {
            assert_ne!(peer, claim.cursor, "a frontier cannot converge with itself");
            assert!(matches!(
                self.node(peer),
                Some(RuntimeNode::RemoteCursor { copy, remote })
                    if *copy == claim.copy && *remote == frontier_port
            ));
            self.join_remote_frontiers(claim.copy, claim.cursor, claim.remote, frontier_port)
        } else {
            match frontier {
                SourceFrontier::Principal {
                    port,
                    node: RuntimeNode::RemoteCursor { .. },
                } => {
                    self.cursor_dependencies.insert(
                        claim.cursor,
                        CursorDependency::SourceCursor {
                            source: claim.source,
                            cursor: port.node(),
                        },
                    );
                    CursorProgress::Blocked
                }
                SourceFrontier::Principal { port, node } => self.materialize_remote_node(
                    claim.copy,
                    claim.cursor,
                    claim.remote,
                    port.node(),
                    node,
                ),
                SourceFrontier::StableAuxiliary {
                    principal_anchors,
                    terminal_pair,
                    ..
                } => {
                    let peer = self.copies.get(&claim.copy).and_then(|state| {
                        principal_anchors
                            .iter()
                            .find_map(|anchor| state.frontiers.get(anchor).copied())
                    });
                    if let Some(peer) = peer {
                        assert_ne!(peer, claim.cursor);
                        self.cursor_dependencies
                            .insert(claim.cursor, CursorDependency::LocalCursor(peer));
                    } else if let Some(pair) = terminal_pair {
                        self.cursor_dependencies.insert(
                            claim.cursor,
                            CursorDependency::SourcePair {
                                source: claim.source,
                                pair,
                            },
                        );
                    }
                    CursorProgress::Blocked
                }
                SourceFrontier::ActiveAuxiliary { entered, partner } => {
                    self.cursor_dependencies.insert(
                        claim.cursor,
                        CursorDependency::SourcePair {
                            source: claim.source,
                            pair: ActivePairKey::new(entered.node(), partner.node()),
                        },
                    );
                    CursorProgress::Blocked
                }
            }
        };

        if progress == CursorProgress::Blocked {
            if let Some(pair) = claim.pair {
                *self.active.get_mut(&pair).unwrap() = ActivePairState::BlockedCursor {
                    cursor: claim.cursor,
                };
            }
        } else if let Some(pair) = claim.pair
            && self.active.get(&pair) == Some(&ActivePairState::Claimed)
        {
            self.active.remove(&pair);
        }
        progress
    }

    pub(in crate::interaction_net::runtime) fn cursor_across(&self, local: Port) -> Option<NodeId> {
        let neighbor = self.neighbor(local)?;
        if !neighbor.is_principal()
            || !matches!(
                self.node(neighbor.node()),
                Some(RuntimeNode::RemoteCursor { .. })
            )
        {
            return None;
        }
        Some(neighbor.node())
    }

    pub(in crate::interaction_net::runtime) fn materialize_remote_node(
        &mut self,
        copy: CopyId,
        cursor: NodeId,
        remote: Port,
        source_node: NodeId,
        node: RuntimeNode<S>,
    ) -> CursorProgress {
        let mut state = self
            .copies
            .remove(&copy)
            .expect("materialized cursor must reference a live copy");
        let node = match node {
            RuntimeNode::Bind => RuntimeNode::Bind,
            RuntimeNode::Fan { identity } => RuntimeNode::Fan {
                identity: self.translate_fan_identity(&mut state, &identity),
            },
            RuntimeNode::Erase => RuntimeNode::Erase,
            RuntimeNode::Data(data) => RuntimeNode::Data(data.clone()),
            RuntimeNode::Operator(operator) => RuntimeNode::Operator(operator),
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => {
                self.copies.insert(copy, state);
                return CursorProgress::Blocked;
            }
        };
        let auxiliaries = match &node {
            RuntimeNode::Bind | RuntimeNode::Fan { .. } => 2,
            RuntimeNode::Operator(_) => 1,
            RuntimeNode::Erase | RuntimeNode::Data(_) => 0,
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => unreachable!(),
        };

        let local = self
            .disconnect(Port::principal(cursor))
            .expect("active remote cursor must face the local net");
        self.remove_node(cursor);
        assert_eq!(state.frontiers.remove(&remote), Some(cursor));

        let target = self.add_node(node);
        self.connect(Port::principal(target), local);
        for index in 1..=auxiliaries {
            let source_anchor = Port::auxiliary(source_node, index);
            let next = self.add_node(RuntimeNode::RemoteCursor {
                copy,
                remote: source_anchor,
            });
            assert!(state.frontiers.insert(source_anchor, next).is_none());
            self.connect(Port::auxiliary(target, index), Port::principal(next));
        }
        self.copies.insert(copy, state);
        CursorProgress::Materialized { node: target }
    }

    pub(in crate::interaction_net::runtime) fn join_remote_frontiers(
        &mut self,
        copy: CopyId,
        cursor: NodeId,
        remote: Port,
        neighbor: Port,
    ) -> CursorProgress {
        let peer = {
            let state = self
                .copies
                .get(&copy)
                .expect("joined cursor must reference a live copy");
            let Some(peer) = state.frontiers.get(&neighbor).copied() else {
                return CursorProgress::Blocked;
            };
            peer
        };
        // A converging frontier may be inspected concurrently from its other
        // end. Leave both frontier records intact until that active-pair claim
        // is released.
        if self
            .active_pair_key(peer)
            .is_some_and(|pair| self.active.get(&pair) == Some(&ActivePairState::Claimed))
        {
            return CursorProgress::Blocked;
        }
        let copy_finished = {
            let state = self
                .copies
                .get_mut(&copy)
                .expect("joined cursor must reference a live copy");
            assert_eq!(state.frontiers.remove(&neighbor), Some(peer));
            assert_eq!(state.frontiers.remove(&remote), Some(cursor));
            state.frontiers.is_empty()
        };
        assert_ne!(
            cursor, peer,
            "a remote wire cannot join one cursor to itself"
        );

        let left = self
            .disconnect(Port::principal(cursor))
            .expect("remote cursor must face the local net");
        self.unschedule_node(peer);
        let right = self
            .disconnect(Port::principal(peer))
            .expect("peer remote cursor must face the local net");
        self.remove_node(cursor);
        self.remove_node(peer);
        self.connect(left, right);
        if copy_finished {
            self.copies.remove(&copy);
        }
        CursorProgress::Joined
    }

    pub(in crate::interaction_net::runtime) fn translate_fan_identity(
        &mut self,
        state: &mut CopyState<S>,
        identity: &FanIdentity,
    ) -> FanIdentity {
        let site = *state.fan_sites.entry(identity.site).or_insert_with(|| {
            let site = FanSite(self.next_fan_site);
            self.next_fan_site = self
                .next_fan_site
                .checked_add(1)
                .expect("interaction-net fan site space exhausted");
            site
        });
        let context = identity
            .context
            .iter()
            .map(|step| DuplicationStep {
                through: self.translate_fan_identity(state, &step.through),
                branch: step.branch,
            })
            .collect::<Vec<_>>();
        FanIdentity {
            site,
            context: Arc::from(context),
        }
    }
}
