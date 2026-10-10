//! Randomized closed-net tests.
//!
//! A seeded generator grows random polarized, connected templates. Each is
//! reduced under several random pair orders with the graph invariants and
//! the polarity type checked after every step. Interaction nets are strongly
//! confluent, so every order must take the same number of steps and reach
//! the same readback. Calls (`Bind >< Data`, `Operator >< Data`) need an
//! evaluator, so they stay inert here.

use std::collections::{BTreeMap, HashMap, VecDeque};

use super::*;
use crate::interaction_net::polarity::PolarityViolation;

/// A small deterministic generator (SplitMix64).
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

type Template = InteractionNet<i32>;

/// An open port awaiting a wire, with the sign it must have.
#[derive(Clone, Copy)]
struct Open {
    port: Port,
    sign: Sign,
}

/// The wiring plan of one generated net, kept so the negative mode can
/// corrupt it before construction.
struct Plan {
    builder: NetBuilder<i32>,
    /// Each wire as `(provider, consumer)`.
    wires: Vec<(Port, Port)>,
    exposed: Port,
    /// Providers whose `+` sign is fixed by their node: data principals,
    /// bind second auxiliaries, and operator results.
    fixed_providers: Vec<Port>,
}

impl Plan {
    fn finish(mut self) -> Result<Template, NetBuildError> {
        for (provider, consumer) in &self.wires {
            self.builder.wire(*provider, *consumer);
        }
        self.builder.try_finish(self.exposed)
    }
}

/// Adds one random node and returns the port meeting `need`, plus its other
/// ports, all with their signs.
fn add_node(
    builder: &mut NetBuilder<i32>,
    random: &mut Seeded,
    need: Sign,
    fixed_providers: &mut Vec<Port>,
) -> (Port, Vec<Open>) {
    let fresh_data = |builder: &mut NetBuilder<i32>, fixed: &mut Vec<Port>| {
        let port = builder.data(fixed.len() as i32 + 1);
        fixed.push(port);
        port
    };
    loop {
        match random.below(5) {
            // Data provides.
            0 if need == Sign::Provides => {
                return (fresh_data(builder, fixed_providers), Vec::new());
            }
            // A bind: choose the role, then which port meets the need.
            1 => {
                let principal = if random.chance(50) {
                    Sign::Provides
                } else {
                    Sign::Consumes
                };
                let ports = builder.bind();
                fixed_providers.push(ports[2]);
                let signs = [principal, Sign::Consumes, Sign::Provides];
                let candidates = (0..3).filter(|&i| signs[i] == need).collect::<Vec<_>>();
                let chosen = candidates[random.below(candidates.len())];
                let others = (0..3)
                    .filter(|&i| i != chosen)
                    .map(|i| Open {
                        port: ports[i],
                        sign: signs[i],
                    })
                    .collect();
                return (ports[chosen], others);
            }
            // A fan, copy or merge; the meeting port is its principal or a
            // branch.
            2 => {
                let fan = builder.push_fan();
                let ports = [
                    Port::principal(fan),
                    Port::auxiliary(fan, 1),
                    Port::auxiliary(fan, 2),
                ];
                let chosen = random.below(3);
                let principal = if chosen == 0 { need } else { need.opposite() };
                let signs = [principal, principal.opposite(), principal.opposite()];
                let others = (0..3)
                    .filter(|&i| i != chosen)
                    .map(|i| Open {
                        port: ports[i],
                        sign: signs[i],
                    })
                    .collect();
                return (ports[chosen], others);
            }
            // An operator: input consumes, result provides.
            3 => {
                let [input, output] = builder.operator(TestOperator::new("increment", |value| {
                    Ok(OperatorYield::Data(value + 1))
                }));
                fixed_providers.push(output);
                return match need {
                    Sign::Consumes => (
                        input,
                        vec![Open {
                            port: output,
                            sign: Sign::Provides,
                        }],
                    ),
                    Sign::Provides => (
                        output,
                        vec![Open {
                            port: input,
                            sign: Sign::Consumes,
                        }],
                    ),
                };
            }
            // An eraser takes either sign.
            4 => return (builder.copy(0).input, Vec::new()),
            _ => {}
        }
    }
}

/// Grows a random polarized, connected net of about `size` nodes.
fn random_plan(random: &mut Seeded, size: usize) -> Plan {
    let mut builder = NetBuilder::new();
    let mut fixed_providers = Vec::new();
    let mut wires = Vec::new();
    let (exposed, mut open) = add_node(&mut builder, random, Sign::Provides, &mut fixed_providers);
    let mut nodes = 1;
    while !open.is_empty() {
        let at = open.swap_remove(random.below(open.len()));
        let partner = open
            .iter()
            .position(|candidate| candidate.sign != at.sign)
            .filter(|_| nodes >= size || random.chance(25));
        let other = match partner {
            Some(index) => open.swap_remove(index).port,
            None if nodes >= size => builder.copy(0).input,
            None => {
                let (port, more) = add_node(
                    &mut builder,
                    random,
                    at.sign.opposite(),
                    &mut fixed_providers,
                );
                open.extend(more);
                nodes += 1;
                port
            }
        };
        wires.push(match at.sign {
            Sign::Provides => (at.port, other),
            Sign::Consumes => (other, at.port),
        });
    }
    Plan {
        builder,
        wires,
        exposed,
        fixed_providers,
    }
}

/// Reduces every structural active pair, in an order chosen by `random`,
/// checking the invariants after each step. Returns the step count, or
/// `None` past the budget.
fn reduce_randomly(
    runtime: &mut RuntimeNet<i32>,
    random: &mut Seeded,
    budget: usize,
) -> Option<usize> {
    runtime.check_invariants();
    for step in 0.. {
        let pairs = runtime
            .active_pairs()
            .filter(|pair| {
                // A pair with no rule becomes stuck rather than ready.
                if !runtime
                    .active
                    .get(pair)
                    .is_some_and(ActivePairState::is_ready)
                {
                    return false;
                }
                let (left, right) = runtime.pair_nodes(*pair).expect("a pair has two nodes");
                !matches!(
                    (runtime.node(left), runtime.node(right)),
                    (Some(RuntimeNode::Bind), Some(RuntimeNode::Data(_)))
                        | (Some(RuntimeNode::Data(_)), Some(RuntimeNode::Bind))
                        | (Some(RuntimeNode::Operator(_)), Some(RuntimeNode::Data(_)))
                        | (Some(RuntimeNode::Data(_)), Some(RuntimeNode::Operator(_)))
                )
            })
            .collect::<Vec<_>>();
        if pairs.is_empty() {
            return Some(step);
        }
        if step == budget {
            return None;
        }
        runtime
            .reduce_pair(pairs[random.below(pairs.len())])
            .expect("a structural pair reduces");
        runtime.check_invariants();
    }
    unreachable!()
}

/// A canonical description of the component reachable from the exposed
/// port: nodes in breadth-first order from the interface, each with its kind
/// and the canonical peer of every port, plus a count of every node kind.
fn readback(runtime: &RuntimeNet<i32>) -> (Vec<String>, BTreeMap<String, usize>) {
    let (lines, _) = reached(runtime, true);
    let mut counts = BTreeMap::new();
    for entry in runtime.nodes.values() {
        *counts.entry(node_kind(&entry.node, true)).or_default() += 1;
    }
    (lines, counts)
}

fn node_kind(node: &RuntimeNode<i32>, fan_identity: bool) -> String {
    match node {
        RuntimeNode::Bind => "Bind".to_owned(),
        RuntimeNode::Fan { identity } if fan_identity => format!("Fan{identity:?}"),
        RuntimeNode::Fan { .. } => "Fan".to_owned(),
        RuntimeNode::Erase => "Erase".to_owned(),
        RuntimeNode::Data(value) => format!("Data({value})"),
        RuntimeNode::Operator(_) => "Operator".to_owned(),
        RuntimeNode::Interface => "Interface".to_owned(),
        other => format!("{other:?}"),
    }
}

/// The readback lines of the component reachable from the exposed port,
/// fan identities included if `fan_identity`, and its nodes in that order.
fn reached(runtime: &RuntimeNet<i32>, fan_identity: bool) -> (Vec<String>, Vec<NodeId>) {
    let start = runtime.exposed().node();
    let mut order = HashMap::from([(start, 0)]);
    let mut queue = VecDeque::from([start]);
    let mut lines = Vec::new();
    let mut nodes = Vec::new();
    while let Some(node) = queue.pop_front() {
        nodes.push(node);
        let entry = runtime.node(node).expect("a reached node exists");
        let mut peers = Vec::new();
        for index in 0..entry.port_count() {
            let peer = runtime.neighbor(Port::new(node, index));
            peers.push(peer.map(|peer| {
                let next = order.len();
                let id = *order.entry(peer.node()).or_insert_with(|| {
                    queue.push_back(peer.node());
                    next
                });
                (id, peer.index())
            }));
        }
        lines.push(format!("{} {peers:?}", node_kind(entry, fan_identity)));
    }
    (lines, nodes)
}

#[test]
fn random_polarized_nets_reduce_to_one_normal_form_in_any_order() {
    let mut generator = Seeded(0x00C0_FFEE);
    let mut normalized = 0;
    for _ in 0..200 {
        let size = 2 + generator.below(24);
        let template = random_plan(&mut generator, size)
            .finish()
            .expect("a generated net is polarized and connected");
        let mut outcomes = Vec::new();
        for order in 0..3 {
            let mut runtime = template.instantiate();
            let mut random = Seeded(generator.next() ^ order);
            let steps = reduce_randomly(&mut runtime, &mut random, 400);
            outcomes.push(steps.map(|steps| (steps, readback(&runtime))));
        }
        assert!(
            outcomes.windows(2).all(|pair| pair[0] == pair[1]),
            "reduction orders disagree: {outcomes:#?}"
        );
        normalized += usize::from(outcomes[0].is_some());
    }
    assert!(
        normalized > 100,
        "too few generated nets normalized: {normalized}"
    );
}

/// A net with no evaluation left copies whole: the copy reads back as its
/// source does, apart from fan sites, which map one to one onto fresh ones.
/// A net with an active pair left, here an inert call, copies through a
/// cursor instead.
#[test]
fn normalized_random_nets_copy_whole_as_they_read_back() {
    let mut generator = Seeded(0x0C09_1E5E);
    let (mut whole, mut active) = (0, 0);
    for _ in 0..200 {
        let size = 2 + generator.below(24);
        let template = random_plan(&mut generator, size)
            .finish()
            .expect("a generated net is polarized and connected");
        let mut source = template.instantiate();
        let mut random = Seeded(generator.next());
        if reduce_randomly(&mut source, &mut random, 400).is_none() {
            continue;
        }
        let copy = source.whole_copy(&DIRECT_RUNTIME_NET_MUTATION_GATEWAY);
        if !source.active.is_empty() {
            assert!(
                copy.is_none(),
                "a net with an active pair copies through a cursor"
            );
            active += 1;
            continue;
        }
        let (source_lines, source_nodes) = reached(&source, false);
        // The interface is not copied.
        let Some(copy) = copy else {
            assert!(
                source_nodes.len() - 1 > 64,
                "a small inert net copies whole"
            );
            continue;
        };
        assert_eq!(copy.len(), source_nodes.len() - 1);

        let mut target = RuntimeNet::<i32>::empty();
        let interface = target.add_node(RuntimeNode::Interface);
        let anchor = Port::auxiliary(interface, 1);
        target.install_whole_copy(copy, SignedPort::new(anchor, Sign::Consumes));
        target.exposed = Some(anchor);
        target.check_invariants();
        let (copy_lines, copy_nodes) = reached(&target, false);
        assert_eq!(copy_lines, source_lines);

        let mut sites = BTreeMap::new();
        for (source_node, copy_node) in source_nodes.iter().zip(&copy_nodes) {
            if let (
                Some(RuntimeNode::Fan { identity: from }),
                Some(RuntimeNode::Fan { identity: to }),
            ) = (source.node(*source_node), target.node(*copy_node))
            {
                assert_eq!(*sites.entry(from.site).or_insert(to.site), to.site);
            }
        }
        let fresh = sites.values().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(fresh.len(), sites.len(), "fan sites map one to one");
        whole += 1;
    }
    assert!(
        whole > 120 && active > 20,
        "copied {whole} whole, {active} active"
    );
}

#[test]
fn corrupted_random_nets_are_rejected_at_construction() {
    let mut generator = Seeded(0x0BAD_5EED);
    let mut rejected = 0;
    for _ in 0..200 {
        let size = 3 + generator.below(20);
        let mut plan = random_plan(&mut generator, size);
        // Rewire one fixed provider to fresh data, a second fixed provider,
        // and its old consumer to an eraser. No free sign can absorb a wire
        // between two fixed providers.
        let Some(index) = plan
            .wires
            .iter()
            .position(|(provider, _)| plan.fixed_providers.contains(provider))
        else {
            continue;
        };
        let (provider, consumer) = plan.wires[index];
        let data = plan.builder.data(0);
        let erase = plan.builder.copy(0).input;
        plan.wires[index] = (provider, data);
        plan.wires.push((erase, consumer));
        match plan.finish() {
            Err(NetBuildError::Polarity(PolarityViolation::Conflict { .. })) => rejected += 1,
            Err(error) => panic!("a corrupted net must be rejected as unpolarized: {error}"),
            Ok(_) => panic!("a corrupted net must not finish"),
        }
    }
    assert!(
        rejected > 50,
        "too few nets had a fixed provider: {rejected}"
    );
}
