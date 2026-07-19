use std::fmt;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

use super::*;
use crate::interaction_net::*;

pub trait TestData: Clone + fmt::Debug + PartialEq + Eq + 'static {}

impl TestData for () {}
impl TestData for i32 {}
impl TestData for &'static str {}

type TestOperatorFn<D> = dyn Fn(&D) -> Result<OperatorYield<D>, Arc<str>> + Send + Sync;

pub struct TestOperator<D: TestData> {
    name: &'static str,
    implementation: Arc<TestOperatorFn<D>>,
}

impl<D: TestData> TestOperator<D> {
    fn new(
        name: &'static str,
        implementation: impl Fn(&D) -> Result<OperatorYield<D>, Arc<str>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            implementation: Arc::new(implementation),
        }
    }

    fn apply(&self, data: &D) -> Result<OperatorYield<D>, Arc<str>> {
        (self.implementation)(data)
    }
}

impl<D: TestData> Clone for TestOperator<D> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            implementation: self.implementation.clone(),
        }
    }
}

impl<D: TestData> fmt::Debug for TestOperator<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("TestOperator")
            .field(&self.name)
            .finish()
    }
}

impl<D: TestData> PartialEq for TestOperator<D> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.implementation, &other.implementation)
    }
}

impl<D: TestData> Eq for TestOperator<D> {}

impl<D: TestData> NetSpecialization for D {
    type Data = D;
    type Operator = TestOperator<D>;
    type WaitToken = u64;
    type StuckReason = Arc<str>;

    fn operator_name(operator: &Self::Operator) -> &str {
        operator.name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StructuredSpecialization;

#[derive(Debug, Clone, PartialEq, Eq)]
struct StructuredStuckReason {
    code: u32,
    detail: Arc<str>,
}

impl fmt::Display for StructuredStuckReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl NetSpecialization for StructuredSpecialization {
    type Data = i32;
    type Operator = ();
    type WaitToken = u64;
    type StuckReason = StructuredStuckReason;

    fn operator_name(_operator: &Self::Operator) -> &str {
        "unreachable test operator"
    }
}

fn finish_claimed_cursor<D: TestData>(
    target: &mut RuntimeNet<D>,
    cursor: NodeId,
) -> CursorProgress {
    let claim = target
        .cursor_claim(cursor)
        .expect("cursor reduction should leave an inspectable claim");
    let frontier = claim
        .source
        .with(|source| source.inspect_source_frontier(claim.remote));
    target.finish_cursor_claim(claim, frontier)
}

fn reduce_next_cursor<D: TestData>(target: &mut RuntimeNet<D>) -> (NodeId, CursorProgress) {
    let Some(Reduction {
        kind:
            ReductionKind::RemoteCursor {
                cursor,
                progress: CursorProgress::Claimed,
            },
        ..
    }) = target.reduce_next()
    else {
        panic!("next reduction should claim a remote cursor");
    };
    let progress = finish_claimed_cursor(target, cursor);
    (cursor, progress)
}

fn reduce_pair_cursor<D: TestData>(
    target: &mut RuntimeNet<D>,
    pair: ActivePairKey,
) -> (NodeId, CursorProgress) {
    let Some(Reduction {
        kind:
            ReductionKind::RemoteCursor {
                cursor,
                progress: CursorProgress::Claimed,
            },
        ..
    }) = target.reduce_pair(pair)
    else {
        panic!("exact reduction should claim a remote cursor");
    };
    let progress = finish_claimed_cursor(target, cursor);
    (cursor, progress)
}

#[test]
fn builder_reports_wiring_errors_without_panicking() {
    let mut net = NetBuilder::<()>::new();
    let [exposed, argument, result] = net.bind();
    let unwired = net.data(());
    net.try_wire(argument, result).unwrap();

    assert_eq!(
        net.try_wire(argument, exposed),
        Err(NetBuildError::PortAlreadyWired(argument))
    );
    assert_eq!(
        net.try_finish(exposed),
        Err(NetBuildError::PortUnwired(unwired))
    );
}

#[test]
fn bind_spine_builds_one_curried_chain() {
    let mut builder = NetBuilder::<()>::new();
    let spine = builder.bind_spine(3);
    let function = builder.data(());
    builder.wire(spine.input, function);
    for argument in spine.arguments {
        let data = builder.data(());
        builder.wire(argument, data);
    }
    let net = builder.finish(spine.result);

    assert_eq!(
        net.nodes()
            .iter()
            .filter(|node| matches!(node, Node::Bind))
            .count(),
        3
    );
    assert_eq!(net.active_pairs().len(), 1);
}

#[test]
fn builder_rejects_a_wired_exposed_port() {
    let mut net = NetBuilder::<()>::new();
    let left = net.data(());
    let right = net.data(());
    net.try_wire(left, right).unwrap();

    assert_eq!(
        net.try_finish(left),
        Err(NetBuildError::ExposedPortWired(left))
    );
}

#[test]
fn builder_rejects_ports_from_another_builder() {
    let mut net = NetBuilder::<()>::new();
    let exposed = net.data(());
    let mut other = NetBuilder::<()>::new();
    other.data(());
    let foreign = other.data(());

    assert_eq!(
        net.try_wire(exposed, foreign),
        Err(NetBuildError::InvalidPort(foreign))
    );
}

#[test]
fn zero_way_copy_is_an_eraser() {
    let mut builder = NetBuilder::<()>::new();
    let copy = builder.copy(0);
    let net = builder.try_finish(copy.input).unwrap();

    assert!(copy.outputs.is_empty());
    assert_eq!(net.nodes(), &[Node::Erase]);
    assert!(net.wires().is_empty());
}

#[test]
fn one_way_copy_is_normalized_out_of_the_template() {
    let mut builder = NetBuilder::<&'static str>::new();
    let copy = builder.copy(1);
    let data = builder.data("value");
    builder.wire(copy.outputs[0], data);
    let net = builder.try_finish(copy.input).unwrap();

    assert_eq!(net.nodes(), &[Node::Data("value")]);
    assert_eq!(net.exposed(), Port::principal(NodeId::from_index(0)));
    assert!(net.wires().is_empty());
}

#[test]
fn many_way_copy_builds_a_balanced_binary_fan_tree() {
    let mut builder = NetBuilder::<()>::new();
    let copy = builder.copy(5);
    for output in copy.outputs.iter().copied() {
        let data = builder.data(());
        builder.wire(output, data);
    }
    let net = builder.try_finish(copy.input).unwrap();

    assert_eq!(copy.outputs.len(), 5);
    assert_eq!(
        net.nodes()
            .iter()
            .filter(|node| matches!(node, Node::Fan { .. }))
            .count(),
        4
    );
    assert_eq!(
        net.nodes()
            .iter()
            .filter(|node| matches!(node, Node::Data(())))
            .count(),
        5
    );
}

fn identity(site: u64) -> FanIdentity {
    FanIdentity::root(FanSite::from_raw(site))
}

fn duplicated_argument_template() -> InteractionNet<()> {
    let mut net = NetBuilder::new();
    let bind = net.push(Node::Bind);
    let fan = net.push_fan();
    let left = net.push(Node::Data(()));
    let right = net.push(Node::Data(()));
    let result = net.push(Node::Data(()));
    net.wire(Port::auxiliary(bind, 1), Port::principal(fan));
    net.wire(Port::auxiliary(fan, 1), Port::principal(left));
    net.wire(Port::auxiliary(fan, 2), Port::principal(right));
    net.wire(Port::auxiliary(bind, 2), Port::principal(result));
    net.finish(Port::principal(bind))
}

#[test]
fn runtime_remembers_a_stable_anchor_for_the_exposed_port() {
    let net = duplicated_argument_template();
    let runtime = net.instantiate();
    assert!(matches!(
        runtime.node(runtime.exposed().node()),
        Some(RuntimeNode::Interface)
    ));
    assert_eq!(runtime.neighbor(runtime.exposed()), Some(net.exposed()));
}

fn fan_pair(left: FanIdentity, right: FanIdentity) -> RuntimeNet<()> {
    let mut runtime = RuntimeNet::empty();
    let left = runtime.add_node(RuntimeNode::Fan { identity: left });
    let right = runtime.add_node(RuntimeNode::Fan { identity: right });
    let left_1 = runtime.add_node(RuntimeNode::Data(()));
    let left_2 = runtime.add_node(RuntimeNode::Data(()));
    let right_1 = runtime.add_node(RuntimeNode::Data(()));
    let right_2 = runtime.add_node(RuntimeNode::Data(()));
    runtime.connect(Port::principal(left), Port::principal(right));
    runtime.connect(Port::auxiliary(left, 1), Port::principal(left_1));
    runtime.connect(Port::auxiliary(left, 2), Port::principal(left_2));
    runtime.connect(Port::auxiliary(right, 1), Port::principal(right_1));
    runtime.connect(Port::auxiliary(right, 2), Port::principal(right_2));
    runtime
}

#[test]
fn identical_fan_histories_join() {
    let fan = identity(3);
    let mut net = fan_pair(fan.clone(), fan.clone());
    let pair = ActivePairKey::new(NodeId::from_index(0), NodeId::from_index(1));
    assert_eq!(
        net.reduce_next(),
        Some(Reduction {
            pair,
            kind: ReductionKind::FanJoin {
                identity: fan.clone()
            }
        })
    );
    assert!(net.node(NodeId::from_index(0)).is_none());
    assert!(net.node(NodeId::from_index(1)).is_none());
    assert_eq!(net.active_pairs().len(), 2);
}

#[test]
fn different_runtime_local_fan_sites_do_not_pair() {
    let left = identity(3);
    let right = identity(4);
    let mut net = fan_pair(left.clone(), right.clone());
    let pair = ActivePairKey::new(NodeId::from_index(0), NodeId::from_index(1));
    assert_eq!(
        net.reduce_next(),
        Some(Reduction {
            pair,
            kind: ReductionKind::FanCommute {
                left: left.clone(),
                right: right.clone()
            }
        })
    );
    assert_eq!(net.active_pairs().len(), 4);
}

#[test]
fn fan_commutation_records_dynamic_duplication_history() {
    let left = identity(3);
    let right = identity(4);
    let mut net = fan_pair(left.clone(), right.clone());
    assert!(matches!(
        net.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::FanCommute { .. },
            ..
        })
    ));
    let residuals = net
        .nodes
        .values()
        .filter_map(|entry| match &entry.node {
            RuntimeNode::Fan { identity } => Some(identity),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(residuals.len(), 4);
    assert!(residuals.iter().all(|fan| fan.context.len() == 1));
}

#[test]
fn ids_ports_and_their_options_are_one_word() {
    assert_eq!(std::mem::size_of::<NodeId>(), std::mem::size_of::<u64>());
    assert_eq!(
        std::mem::size_of::<Option<NodeId>>(),
        std::mem::size_of::<u64>()
    );
    assert_eq!(std::mem::size_of::<Port>(), std::mem::size_of::<u64>());
    assert_eq!(
        std::mem::size_of::<Option<Port>>(),
        std::mem::size_of::<u64>()
    );
}

#[test]
fn active_pair_key_is_the_lower_node_id_and_recovers_its_partner() {
    let mut net = RuntimeNet::<()>::empty();
    let lower = net.add_node(RuntimeNode::Data(()));
    let higher = net.add_node(RuntimeNode::Data(()));
    net.connect(Port::principal(higher), Port::principal(lower));

    let key = *net.ready_pairs().first().unwrap();
    assert_eq!(key.node(), lower);
    assert_eq!(net.pair_nodes(key), Some((lower, higher)));
}

#[test]
fn claimed_and_stuck_pairs_remain_in_the_active_tree() {
    let mut calls = RuntimeNet::<()>::empty();
    let bind = calls.add_node(RuntimeNode::Bind);
    let data = calls.add_node(RuntimeNode::Data(()));
    calls.connect(Port::principal(bind), Port::principal(data));
    let call_pair = ActivePairKey::new(bind, data);
    assert_eq!(
        calls.reduce_next(),
        Some(Reduction {
            pair: call_pair,
            kind: ReductionKind::Call { bind, data },
        })
    );
    assert!(calls.ready_pairs().is_empty());
    assert_eq!(
        calls.active.get(&call_pair),
        Some(&ActivePairState::Claimed)
    );
    assert_eq!(calls.reduce_next(), None);

    let mut stuck = RuntimeNet::<()>::empty();
    let left = stuck.add_node(RuntimeNode::Data(()));
    let right = stuck.add_node(RuntimeNode::Data(()));
    stuck.connect(Port::principal(left), Port::principal(right));
    let stuck_pair = ActivePairKey::new(left, right);
    assert_eq!(
        stuck.reduce_next(),
        Some(Reduction {
            pair: stuck_pair,
            kind: ReductionKind::Stuck,
        })
    );
    assert!(stuck.ready_pairs().is_empty());
    assert_eq!(
        stuck.stuck_pairs().collect::<Vec<_>>(),
        vec![StuckPair {
            pair: stuck_pair,
            reason: StuckReason::NoRule,
        }]
    );
    assert_eq!(stuck.reduce_next(), None);
}

#[test]
fn shared_runtime_waiters_resume_when_a_claimed_pair_is_released() {
    let mut net = RuntimeNet::<()>::empty();
    let bind = net.add_node(RuntimeNode::Bind);
    let data = net.add_node(RuntimeNode::Data(()));
    net.connect(Port::principal(bind), Port::principal(data));
    let shared = SharedRuntimeNet::new(net);
    let reduction = shared
        .with_mut(RuntimeNet::reduce_next)
        .expect("bind-data pair should be claimed");
    let ReductionKind::Call { bind, data } = reduction.kind else {
        panic!("expected a claimed call");
    };
    let call = Call {
        pair: reduction.pair,
        bind,
        data,
    };
    let (claimed, version) = shared.with_version(|net| net.pair_is_claimed(call.pair));
    assert!(claimed);

    let barrier = Arc::new(Barrier::new(2));
    let waiter_barrier = barrier.clone();
    let waiter_net = shared.clone();
    let (sender, receiver) = mpsc::channel();
    let waiter = thread::spawn(move || {
        waiter_barrier.wait();
        waiter_net.wait_for_change(version);
        sender.send(()).expect("test receiver should remain open");
    });
    barrier.wait();

    shared.with_mut(|net| net.fail_claimed_call(call, Arc::from("released for test")));
    receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("releasing a claimed pair should wake shared runtime waiters");
    waiter.join().expect("runtime waiter should not panic");
}

#[test]
fn blocked_call_requires_its_current_wait_token_to_be_reclaimed() {
    let mut net = RuntimeNet::<()>::empty();
    let bind = net.add_node(RuntimeNode::Bind);
    let data = net.add_node(RuntimeNode::Data(()));
    net.connect(Port::principal(bind), Port::principal(data));
    let pair = ActivePairKey::new(bind, data);
    let reduction = net.reduce_next().expect("bind-data must claim a call");
    let ReductionKind::Call { bind, data } = reduction.kind else {
        panic!("expected a claimed call");
    };
    let call = Call { pair, bind, data };

    net.block_claimed_call(call, 17);

    assert_eq!(net.blocked_call(pair), Some(BlockedCall { pair, wait: 17 }));
    assert_eq!(
        net.blocked_calls().collect::<Vec<_>>(),
        vec![BlockedCall { pair, wait: 17 }]
    );
    assert!(!net.retry_blocked_call(call, &16));
    assert_eq!(net.blocked_call(pair).unwrap().wait, 17);
    assert!(net.retry_blocked_call(call, &17));
    assert_eq!(net.claim_call(call), Some(()));
    assert!(net.principals_connect(pair));
}

#[test]
fn specialization_failure_remains_structured_in_the_stuck_pair() {
    let mut net = RuntimeNet::<StructuredSpecialization>::empty();
    let bind = net.add_node(RuntimeNode::Bind);
    let data = net.add_node(RuntimeNode::Data(7));
    net.connect(Port::principal(bind), Port::principal(data));
    let pair = ActivePairKey::new(bind, data);
    let reduction = net.reduce_next().expect("bind-data must claim a call");
    let ReductionKind::Call { bind, data } = reduction.kind else {
        panic!("expected a claimed call");
    };
    let call = Call { pair, bind, data };
    let error = StructuredStuckReason {
        code: 42,
        detail: Arc::from("not callable"),
    };
    net.fail_claimed_call(call, error.clone());

    assert_eq!(error.code, 42);
    assert_eq!(
        net.stuck_reason(pair).cloned(),
        Some(StuckReason::Specialization(error))
    );
}

#[test]
fn claimed_callable_data_lowers_to_an_explicit_operator_bind() {
    let mut net = RuntimeNet::<i32>::empty();
    let application = net.add_node(RuntimeNode::Bind);
    let callable = net.add_node(RuntimeNode::Data(0));
    let argument = net.add_node(RuntimeNode::Data(41));
    let interface = net.add_node(RuntimeNode::Interface);
    let result = Port::auxiliary(interface, 1);
    net.connect(Port::principal(application), Port::principal(callable));
    net.connect(Port::auxiliary(application, 1), Port::principal(argument));
    net.connect(Port::auxiliary(application, 2), result);

    let reduction = net.reduce_next().expect("bind-data must block as a call");
    let ReductionKind::Call { bind, data } = reduction.kind else {
        panic!("expected a claimed call");
    };
    let call = Call {
        pair: reduction.pair,
        bind,
        data,
    };
    assert_eq!(net.claim_call(call), Some(0));
    assert_eq!(net.active.get(&call.pair), Some(&ActivePairState::Claimed));

    net.resume_claimed_call_with_operator(
        call,
        TestOperator::new("increment", |value| Ok(OperatorYield::Data(value + 1))),
    );
    assert_ne!(net.active.get(&call.pair), Some(&ActivePairState::Claimed));
    assert!(matches!(
        net.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::BindJoin,
            ..
        })
    ));
    let operator_call = match net.reduce_next() {
        Some(Reduction {
            kind: ReductionKind::OperatorCall { operator, data },
            pair,
        }) => OperatorCall {
            pair,
            operator,
            data,
        },
        other => panic!("expected operator call, got {other:?}"),
    };
    let (operator, data) = net.operator_call_parts(operator_call);
    net.complete_operator_call(operator_call, operator.apply(&data).unwrap());
    assert_eq!(net.interface_data(result), Some(&42));
}

fn operator_call_net(
    operator: TestOperator<i32>,
    input: i32,
) -> (RuntimeNet<i32>, OperatorCall, Port) {
    let mut net = RuntimeNet::<i32>::empty();
    let host = net.add_node(RuntimeNode::Operator(operator));
    let data = net.add_node(RuntimeNode::Data(input));
    let interface = net.add_node(RuntimeNode::Interface);
    let result = Port::auxiliary(interface, 1);
    net.connect(Port::principal(host), Port::principal(data));
    net.connect(Port::auxiliary(host, 1), result);
    let pair = ActivePairKey::new(host, data);
    assert!(matches!(
        net.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::OperatorCall { .. },
            ..
        })
    ));
    (
        net,
        OperatorCall {
            pair,
            operator: host,
            data,
        },
        result,
    )
}

#[test]
fn operator_consumes_data_and_emits_data() {
    let (mut net, call, result) = operator_call_net(
        TestOperator::new("increment", |value| Ok(OperatorYield::Data(value + 1))),
        41,
    );
    let (operator, data) = net.operator_call_parts(call);
    let outcome = operator.apply(&data).unwrap();

    net.complete_operator_call(call, outcome);

    assert_eq!(net.interface_data(result), Some(&42));
    assert!(!net.active.contains_key(&call.pair));
}

#[test]
fn returned_operator_is_wrapped_as_a_unary_function() {
    let next = TestOperator::new("increment", |value| Ok(OperatorYield::Data(value + 1)));
    let (mut net, call, result) = operator_call_net(
        TestOperator::new("curry", move |_| Ok(OperatorYield::Operator(next.clone()))),
        0,
    );
    let (operator, data) = net.operator_call_parts(call);
    let outcome = operator.apply(&data).unwrap();

    let bind = net.complete_operator_call(call, outcome);

    assert_eq!(net.interface_neighbor(result), Some(Port::principal(bind)));
    let host = net.port_neighbor(Port::auxiliary(bind, 1)).unwrap();
    assert!(matches!(
        net.node(host.node()),
        Some(RuntimeNode::Operator(_))
    ));
    assert_eq!(
        net.port_neighbor(Port::auxiliary(bind, 2)),
        Some(Port::auxiliary(host.node(), 1))
    );
}

#[test]
fn operator_error_preserves_the_stuck_active_pair() {
    let (mut failed, call, _) = operator_call_net(
        TestOperator::new("failed", |_| Err(Arc::from("invalid input"))),
        0,
    );
    let (operator, data) = failed.operator_call_parts(call);
    let Err(error) = operator.apply(&data) else {
        panic!("operator should fail");
    };
    failed.fail_operator_call(call, error);
    assert!(matches!(
        failed.active.get(&call.pair),
        Some(ActivePairState::Stuck(_))
    ));
    assert_eq!(
        failed.stuck_pairs().collect::<Vec<_>>(),
        vec![StuckPair {
            pair: call.pair,
            reason: StuckReason::Specialization(Arc::from("invalid input")),
        }]
    );
    assert!(failed.principals_connect(call.pair));
}

#[test]
fn active_tree_tracks_every_principal_connection_once() {
    let mut net = RuntimeNet::<()>::empty();
    let bind = net.add_node(RuntimeNode::Bind);
    let call_data = net.add_node(RuntimeNode::Data(()));
    let stuck_left = net.add_node(RuntimeNode::Data(()));
    let stuck_right = net.add_node(RuntimeNode::Data(()));
    let ready_fan = net.add_node(RuntimeNode::Fan {
        identity: identity(0),
    });
    let ready_data = net.add_node(RuntimeNode::Data(()));
    net.connect(Port::principal(bind), Port::principal(call_data));
    net.connect(Port::principal(stuck_left), Port::principal(stuck_right));
    net.connect(Port::principal(ready_fan), Port::principal(ready_data));

    assert!(matches!(
        net.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::Call { .. },
            ..
        })
    ));
    assert!(matches!(
        net.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::Stuck,
            ..
        })
    ));

    let mut graph_pairs = net
        .nodes
        .keys()
        .filter_map(|node| {
            let neighbor = net.neighbor(Port::principal(*node))?;
            (neighbor.is_principal() && node.get() < neighbor.node().get())
                .then_some((node.get(), neighbor.node().get()))
        })
        .collect::<Vec<_>>();
    graph_pairs.sort_unstable();

    let mut scheduled_pairs = net
        .active_pairs()
        .map(|pair| {
            let (left, right) = net.pair_nodes(pair).unwrap();
            (left.get(), right.get())
        })
        .collect::<Vec<_>>();
    scheduled_pairs.sort_unstable();

    assert_eq!(scheduled_pairs, graph_pairs);
}

fn source_requiring_one_reduction() -> InteractionNet<&'static str> {
    let mut net = NetBuilder::new();
    let left = net.push(Node::Bind);
    let right = net.push(Node::Bind);
    let left_result = net.push(Node::Data("left-result"));
    let exposed_result = net.push(Node::Data("exposed-result"));
    let right_result = net.push(Node::Data("right-result"));
    net.wire(Port::principal(left), Port::principal(right));
    net.wire(Port::auxiliary(left, 2), Port::principal(left_result));
    net.wire(Port::auxiliary(right, 1), Port::principal(exposed_result));
    net.wire(Port::auxiliary(right, 2), Port::principal(right_result));
    net.finish(Port::auxiliary(left, 1))
}

fn target_waiting_on(source: SharedRuntimeNet<&'static str>) -> RuntimeNet<&'static str> {
    let mut target = RuntimeNet::empty();
    let local = target.add_node(RuntimeNode::Data("local"));
    let cursor = target.begin_copy(source);
    target.connect(Port::principal(local), Port::principal(cursor));
    target
}

#[test]
fn remote_cursor_exposes_source_progress_without_holding_nested_locks() {
    let source = source_requiring_one_reduction().instantiate_shared();
    let mut first = target_waiting_on(source.clone());

    let (_, progress) = reduce_next_cursor(&mut first);
    assert_eq!(progress, CursorProgress::Blocked);
    source.with_mut(|runtime| {
        assert!(matches!(
            runtime.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));
    });
    let cursor = first
        .blocked_cursors()
        .values()
        .next()
        .expect("cursor should remain blocked")
        .cursor;
    assert!(first.retry_blocked_cursor(cursor));
    assert!(matches!(
        reduce_next_cursor(&mut first).1,
        CursorProgress::Materialized { .. }
    ));
    // Driving demand advances only one source reduction. Newly exposed,
    // unrelated pairs remain lazy in the shared source.
    assert_eq!(source.with(|runtime| runtime.ready_pairs().len()), 1);

    let mut second = target_waiting_on(source);
    assert!(matches!(
        reduce_next_cursor(&mut second).1,
        CursorProgress::Materialized { .. }
    ));
}

#[test]
fn active_source_call_is_a_dependency_and_is_never_copied() {
    let mut source: RuntimeNet<&'static str> = RuntimeNet::empty();
    let bind = source.add_node(RuntimeNode::Bind);
    let callable = source.add_node(RuntimeNode::Data("callable"));
    let result = source.add_node(RuntimeNode::Data("result"));
    source.connect(Port::principal(bind), Port::principal(callable));
    source.connect(Port::auxiliary(bind, 2), Port::principal(result));
    let exposed = source.add_interface(Port::auxiliary(bind, 1));
    source.exposed = Some(exposed);
    let pair = ActivePairKey::new(bind, callable);
    assert!(matches!(
        source.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::Call { .. },
            ..
        })
    ));
    let source = SharedRuntimeNet::new(source);

    let mut target = target_waiting_on(source.clone());
    let (cursor, progress) = reduce_next_cursor(&mut target);
    assert_eq!(progress, CursorProgress::Blocked);
    let dependency = target.cursor_dependency(cursor).unwrap();
    let CursorDependency::SourcePair {
        source: dependency_source,
        pair: dependency_pair,
    } = dependency
    else {
        panic!("active source call should remain an exact source dependency");
    };
    assert!(dependency_source.ptr_eq(&source));
    assert_eq!(dependency_pair, pair);
    source.with(|source| {
        assert_eq!(source.active.get(&pair), Some(&ActivePairState::Claimed));
    });
    assert!(
        !target
            .nodes
            .values()
            .any(|entry| matches!(entry.node, RuntimeNode::Bind))
    );
}

#[test]
fn layered_cursor_reports_and_follows_an_exact_dependency() {
    let mut leaf = NetBuilder::new();
    let data = leaf.data("leaf");
    let leaf = leaf.finish(data).instantiate_shared();

    let mut middle = RuntimeNet::empty();
    let middle_cursor = middle.begin_copy(leaf);
    let exposed = middle.add_interface(Port::principal(middle_cursor));
    middle.exposed = Some(exposed);
    let middle = SharedRuntimeNet::new(middle);

    let mut outer = target_waiting_on(middle.clone());
    let (outer_cursor, progress) = reduce_next_cursor(&mut outer);
    assert_eq!(progress, CursorProgress::Blocked);
    let dependency = outer
        .cursor_dependency(outer_cursor)
        .expect("layered cursor should retain an exact dependency");
    let CursorDependency::SourceCursor { source, cursor } = dependency else {
        panic!("layered cursor should point to its exact source cursor");
    };
    assert!(source.ptr_eq(&middle));
    assert_eq!(cursor, middle_cursor);

    assert!(matches!(
        middle.with_mut(|runtime| runtime.claim_dependent_cursor(middle_cursor)),
        Some(CursorProgress::Claimed)
    ));
    assert!(matches!(
        middle.advance_claimed_cursor(middle_cursor),
        Some(CursorProgress::Materialized { .. })
    ));
    assert!(outer.retry_blocked_cursor(outer_cursor));
    assert!(matches!(
        reduce_next_cursor(&mut outer).1,
        CursorProgress::Materialized { .. }
    ));
}

#[test]
fn auxiliary_cursor_drives_the_local_cursor_facing_the_principal() {
    let mut source: RuntimeNet<&'static str> = RuntimeNet::empty();
    let root = source.add_node(RuntimeNode::Bind);
    let host = source.add_node(RuntimeNode::Operator(TestOperator::new(
        "identity",
        |data| Ok(OperatorYield::Data(*data)),
    )));
    let exposed = source.add_interface(Port::principal(root));
    source.connect(Port::auxiliary(root, 1), Port::principal(host));
    source.connect(Port::auxiliary(root, 2), Port::auxiliary(host, 1));
    source.exposed = Some(exposed);
    let source = SharedRuntimeNet::new(source);

    let mut target = RuntimeNet::empty();
    let root_cursor = target.begin_copy(source);
    let target_exposed = target.add_interface(Port::principal(root_cursor));
    assert_eq!(
        target.demand_interface(target_exposed),
        Some(CursorProgress::Claimed)
    );
    assert!(matches!(
        finish_claimed_cursor(&mut target, root_cursor),
        CursorProgress::Materialized { .. }
    ));

    let state = target.copies.values().next().unwrap();
    let argument_cursor = state.frontiers[&Port::auxiliary(root, 1)];
    let result_cursor = state.frontiers[&Port::auxiliary(root, 2)];
    assert_eq!(
        target.claim_dependent_cursor(result_cursor),
        Some(CursorProgress::Claimed)
    );
    assert_eq!(
        finish_claimed_cursor(&mut target, result_cursor),
        CursorProgress::Blocked
    );
    assert!(matches!(
        target.cursor_dependency(result_cursor),
        Some(CursorDependency::LocalCursor(cursor)) if cursor == argument_cursor
    ));
    assert_eq!(
        target.claim_dependent_cursor(argument_cursor),
        Some(CursorProgress::Claimed)
    );
    assert!(matches!(
        finish_claimed_cursor(&mut target, argument_cursor),
        CursorProgress::Materialized { .. }
    ));
    assert_eq!(
        target.claim_dependent_cursor(result_cursor),
        Some(CursorProgress::Claimed)
    );
    assert_eq!(
        finish_claimed_cursor(&mut target, result_cursor),
        CursorProgress::Joined
    );
}

#[test]
fn auxiliary_cursor_traces_a_principal_chain_to_an_exact_source_pair() {
    let mut source: RuntimeNet<&'static str> = RuntimeNet::empty();
    let root = source.add_node(RuntimeNode::Bind);
    let middle = source.add_node(RuntimeNode::Bind);
    let upstream = source.add_node(RuntimeNode::Bind);
    let callable = source.add_node(RuntimeNode::Data("callable"));
    source.connect(Port::auxiliary(root, 2), Port::auxiliary(middle, 2));
    source.connect(Port::principal(middle), Port::auxiliary(upstream, 2));
    source.connect(Port::principal(upstream), Port::principal(callable));
    let exposed = source.add_interface(Port::principal(root));
    source.exposed = Some(exposed);
    let pair = ActivePairKey::new(upstream, callable);
    assert!(matches!(
        source.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::Call { .. },
            ..
        })
    ));
    let source = SharedRuntimeNet::new(source);

    let mut target = RuntimeNet::empty();
    let root_cursor = target.begin_copy(source.clone());
    let target_exposed = target.add_interface(Port::principal(root_cursor));
    assert_eq!(
        target.demand_interface(target_exposed),
        Some(CursorProgress::Claimed)
    );
    assert!(matches!(
        finish_claimed_cursor(&mut target, root_cursor),
        CursorProgress::Materialized { .. }
    ));

    let cursor = target.copies.values().next().unwrap().frontiers[&Port::auxiliary(root, 2)];
    assert_eq!(
        target.claim_dependent_cursor(cursor),
        Some(CursorProgress::Claimed)
    );
    assert_eq!(
        finish_claimed_cursor(&mut target, cursor),
        CursorProgress::Blocked
    );
    assert!(matches!(
        target.cursor_dependency(cursor),
        Some(CursorDependency::SourcePair {
            source: dependency_source,
            pair: dependency_pair,
        }) if dependency_source.ptr_eq(&source) && dependency_pair == pair
    ));
}

#[test]
fn materializing_a_root_creates_lazy_auxiliary_cursors() {
    let source = duplicated_argument_template().instantiate_shared();
    let source_nodes = source.with(|runtime| runtime.nodes.len());
    let mut target = RuntimeNet::empty();
    let local = target.add_node(RuntimeNode::Data(()));
    let cursor = target.begin_copy(source.clone());
    target.connect(Port::principal(local), Port::principal(cursor));

    assert!(matches!(
        reduce_next_cursor(&mut target).1,
        CursorProgress::Materialized { .. }
    ));
    let cursors = target
        .nodes
        .values()
        .filter(|entry| matches!(entry.node, RuntimeNode::RemoteCursor { .. }))
        .count();
    assert_eq!(cursors, 2);
    assert_eq!(source.with(|runtime| runtime.nodes.len()), source_nodes);
}

#[test]
fn resuming_a_call_materializes_only_the_root_bind() {
    let source = duplicated_argument_template().instantiate_shared();
    let mut caller = RuntimeNet::empty();
    let bind = caller.add_node(RuntimeNode::Bind);
    let function = caller.add_node(RuntimeNode::Data(()));
    let argument = caller.add_node(RuntimeNode::Data(()));
    let result = caller.add_node(RuntimeNode::Data(()));
    caller.connect(Port::principal(bind), Port::principal(function));
    caller.connect(Port::auxiliary(bind, 1), Port::principal(argument));
    caller.connect(Port::auxiliary(bind, 2), Port::principal(result));

    let Some(Reduction {
        pair,
        kind: ReductionKind::Call { bind, data },
    }) = caller.reduce_next()
    else {
        panic!("bind-data must block as a call");
    };
    let call = Call { pair, bind, data };
    caller.resume_claimed_call_with_copy(call, source);
    assert!(matches!(
        reduce_next_cursor(&mut caller).1,
        CursorProgress::Materialized { .. }
    ));
    assert!(matches!(
        caller.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::BindJoin,
            ..
        })
    ));
    assert_eq!(
        caller
            .nodes
            .values()
            .filter(|entry| matches!(entry.node, RuntimeNode::RemoteCursor { .. }))
            .count(),
        2
    );
}

#[test]
fn converging_frontiers_join_without_leaving_a_stale_cursor_pair() {
    let mut template = NetBuilder::<()>::new();
    let root = template.push(Node::Bind);
    template.wire(Port::auxiliary(root, 1), Port::auxiliary(root, 2));
    let source = template.finish(Port::principal(root)).instantiate_shared();

    let mut caller = RuntimeNet::empty();
    let bind = caller.add_node(RuntimeNode::Bind);
    let function = caller.add_node(RuntimeNode::Data(()));
    let left = caller.add_node(RuntimeNode::Data(()));
    let right = caller.add_node(RuntimeNode::Data(()));
    caller.connect(Port::principal(bind), Port::principal(function));
    caller.connect(Port::auxiliary(bind, 1), Port::principal(left));
    caller.connect(Port::auxiliary(bind, 2), Port::principal(right));

    let Some(Reduction {
        pair,
        kind: ReductionKind::Call { bind, data },
    }) = caller.reduce_next()
    else {
        panic!("bind-data must become a call");
    };
    let call = Call { pair, bind, data };
    caller.resume_claimed_call_with_copy(call, source);
    assert!(matches!(
        reduce_next_cursor(&mut caller).1,
        CursorProgress::Materialized { .. }
    ));
    caller.reduce_next();
    assert!(matches!(
        reduce_next_cursor(&mut caller).1,
        CursorProgress::Joined
    ));
    assert!(caller.copies.is_empty());
    assert!(matches!(
        caller.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::Stuck,
            ..
        })
    ));
    assert!(caller.reduce_next().is_none());
}

#[test]
fn converging_frontier_waits_for_a_claimed_peer() {
    let mut template = NetBuilder::<()>::new();
    let root = template.push(Node::Bind);
    template.wire(Port::auxiliary(root, 1), Port::auxiliary(root, 2));
    let source = template.finish(Port::principal(root)).instantiate_shared();

    let mut caller = RuntimeNet::empty();
    let bind = caller.add_node(RuntimeNode::Bind);
    let function = caller.add_node(RuntimeNode::Data(()));
    let left = caller.add_node(RuntimeNode::Data(()));
    let right = caller.add_node(RuntimeNode::Data(()));
    caller.connect(Port::principal(bind), Port::principal(function));
    caller.connect(Port::auxiliary(bind, 1), Port::principal(left));
    caller.connect(Port::auxiliary(bind, 2), Port::principal(right));

    let Some(Reduction {
        pair,
        kind: ReductionKind::Call { bind, data },
    }) = caller.reduce_next()
    else {
        panic!("bind-data must become a call");
    };
    let call = Call { pair, bind, data };
    caller.resume_claimed_call_with_copy(call, source);
    assert!(matches!(
        reduce_next_cursor(&mut caller).1,
        CursorProgress::Materialized { .. }
    ));
    assert!(matches!(
        caller.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::BindJoin,
            ..
        })
    ));

    let mut claims = Vec::new();
    for _ in 0..2 {
        let Some(Reduction {
            kind:
                ReductionKind::RemoteCursor {
                    cursor,
                    progress: CursorProgress::Claimed,
                },
            ..
        }) = caller.reduce_next()
        else {
            panic!("each converging cursor should be independently claimable");
        };
        let claim = caller.cursor_claim(cursor).unwrap();
        let frontier = claim
            .source
            .with(|source| source.inspect_source_frontier(claim.remote));
        claims.push((claim, frontier));
    }

    let (first_claim, first_frontier) = claims.remove(0);
    assert_eq!(
        caller.finish_cursor_claim(first_claim, first_frontier),
        CursorProgress::Blocked
    );
    let (second_claim, second_frontier) = claims.remove(0);
    assert_eq!(
        caller.finish_cursor_claim(second_claim, second_frontier),
        CursorProgress::Joined
    );
    assert!(caller.copies.is_empty());
    assert!(caller.blocked_cursors().is_empty());
    assert!(
        caller
            .active
            .values()
            .all(|state| state != &ActivePairState::Claimed)
    );
}

#[test]
fn separate_logical_copies_rebase_fans_to_distinct_local_sites() {
    let mut template = NetBuilder::<()>::new();
    let fan = template.push_fan();
    let left = template.push(Node::Data(()));
    let right = template.push(Node::Data(()));
    template.wire(Port::auxiliary(fan, 1), Port::principal(left));
    template.wire(Port::auxiliary(fan, 2), Port::principal(right));
    let source = template.finish(Port::principal(fan)).instantiate_shared();

    let mut target = RuntimeNet::empty();
    let mut cursor_pairs = Vec::new();
    for _ in 0..2 {
        let local = target.add_node(RuntimeNode::Data(()));
        let cursor = target.begin_copy(source.clone());
        target.connect(Port::principal(local), Port::principal(cursor));
        cursor_pairs.push(ActivePairKey::new(local, cursor));
    }
    for pair in cursor_pairs {
        assert!(matches!(
            reduce_pair_cursor(&mut target, pair).1,
            CursorProgress::Materialized { .. }
        ));
    }
    let mut sites = target
        .nodes
        .values()
        .filter_map(|entry| match &entry.node {
            RuntimeNode::Fan { identity } => Some(identity.site.get()),
            _ => None,
        })
        .collect::<Vec<_>>();
    sites.sort_unstable();
    assert_eq!(sites, vec![0, 1]);
}

#[test]
fn erasing_a_remote_cursor_materializes_then_uses_ordinary_erasure() {
    let source = duplicated_argument_template().instantiate_shared();
    let source_nodes = source.with(|runtime| runtime.nodes.len());
    let mut target = RuntimeNet::empty();
    let eraser = target.add_node(RuntimeNode::Erase);
    let cursor = target.begin_copy(source.clone());
    target.connect(Port::principal(eraser), Port::principal(cursor));

    assert!(matches!(
        reduce_next_cursor(&mut target).1,
        CursorProgress::Materialized { .. }
    ));
    assert!(matches!(
        target.reduce_next(),
        Some(Reduction {
            kind: ReductionKind::Erase,
            ..
        })
    ));
    assert_eq!(source.with(|runtime| runtime.nodes.len()), source_nodes);
    assert!(!target.copies.is_empty());
}

#[test]
fn removed_node_ids_are_not_reused() {
    let mut net = RuntimeNet::<()>::empty();
    let first = net.add_node(RuntimeNode::Data(()));
    let second = net.add_node(RuntimeNode::Data(()));
    assert!(matches!(net.remove_node(first), RuntimeNode::Data(())));
    let third = net.add_node(RuntimeNode::Data(()));
    assert_eq!(first.get(), 0);
    assert_eq!(second.get(), 1);
    assert_eq!(third.get(), 2);
}
