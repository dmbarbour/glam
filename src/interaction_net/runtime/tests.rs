use std::fmt;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

use super::*;
use crate::interaction_net::builder::{NetBuildError, NetBuilder};

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
}

fn finish_claimed_cursor<S: NetSpecialization>(
    target: &mut RuntimeNet<S>,
    cursor: NodeId,
) -> CursorProgress {
    let claim = target
        .cursor_claim(cursor)
        .expect("cursor reduction should leave an inspectable claim");
    let frontier = claim.source.inspect_source_frontier(claim.remote);
    target.finish_cursor_claim(claim, frontier)
}

fn reduce_next_cursor<S: NetSpecialization>(
    target: &mut RuntimeNet<S>,
) -> (NodeId, CursorProgress) {
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

fn claim_test_cursor<S: NetSpecialization>(
    target: &mut RuntimeNet<S>,
    cursor: NodeId,
) -> Option<CursorProgress> {
    matches!(target.node(cursor), Some(RuntimeNode::RemoteCursor { .. }))
        .then(|| target.begin_cursor_claim(cursor, None))
        .flatten()
}

fn claim_test_interface_cursor<S: NetSpecialization>(
    target: &mut RuntimeNet<S>,
    interface: Port,
) -> Option<CursorProgress> {
    target.assert_interface(interface);
    let neighbor = target.neighbor(interface)?;
    (neighbor.is_principal()
        && matches!(
            target.node(neighbor.node()),
            Some(RuntimeNode::RemoteCursor { .. })
        ))
    .then(|| target.begin_cursor_claim(neighbor.node(), None))
    .flatten()
}

fn remove_unwired_test_copy<S: NetSpecialization>(target: &SharedRuntimeNet<S>, cursor: NodeId) {
    target.with_mut(|runtime| {
        let copy = match runtime.node(cursor) {
            Some(RuntimeNode::RemoteCursor { copy, .. }) => *copy,
            _ => panic!("logical copy should install a remote cursor"),
        };
        assert!(runtime.copies.remove(&copy).is_some());
        assert!(matches!(
            runtime.remove_node(cursor),
            RuntimeNode::RemoteCursor { .. }
        ));
    });
}

fn pairless_cursor_dependency_fixture() -> (SharedRuntimeNet<()>, NodeId, CursorDependency<()>) {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    let dependency = target.begin_copy(source.prepare_copy_source());
    assert!(target.ensure_pairless_cursor_obligation(cursor));
    assert!(target.claim_pairless_cursor_obligation(cursor));
    let expected = CursorDependency::LocalCursor(dependency);
    assert!(target.block_pairless_cursor_obligation(cursor, expected.clone()));
    (SharedRuntimeNet::new(target), cursor, expected)
}

fn pair_owned_cursor_dependency_fixture() -> (SharedRuntimeNet<()>, NodeId, CursorDependency<()>) {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    let dependency = target.begin_copy(source.prepare_copy_source());
    let local = target.add_node(RuntimeNode::Data(()));
    target.connect(Port::principal(cursor), Port::principal(local));
    let pair = ActivePairKey::new(cursor, local);
    let expected = CursorDependency::LocalCursor(dependency);
    target.active.insert(
        pair,
        ActivePairState::BlockedCursor {
            cursor,
            blockage: CursorBlockage::Dependency(expected.clone()),
        },
    );
    (SharedRuntimeNet::new(target), cursor, expected)
}

fn reduce_pair_cursor<S: NetSpecialization>(
    target: &mut RuntimeNet<S>,
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
    let (claimed, revisions) = shared.with_revisions(|net| net.pair_is_claimed(call.pair));
    assert!(claimed);

    let barrier = Arc::new(Barrier::new(2));
    let waiter_barrier = barrier.clone();
    let waiter_net = shared.clone();
    let (sender, receiver) = mpsc::channel();
    let waiter = thread::spawn(move || {
        waiter_barrier.wait();
        waiter_net.wait_for_disturbance(revisions.disturbance_epoch());
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
fn conditional_runtime_mutation_publishes_only_new_cursor_obligations() {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    let target = SharedRuntimeNet::new(target);

    let (initial_topology, initial_disturbance) = target.revisions();
    assert_eq!(initial_topology, initial_disturbance);

    let inserted = target.with_conditional_mut(|runtime| {
        if runtime.ensure_pairless_cursor_obligation(cursor) {
            RuntimeNetMutation::Changed(true)
        } else {
            RuntimeNetMutation::Unchanged(false)
        }
    });
    assert!(inserted);
    target.with(RuntimeNet::assert_cursor_obligation_invariants);
    let (inserted_topology, inserted_disturbance) = target.revisions();
    assert_eq!(inserted_topology, initial_topology + 1);
    assert_eq!(inserted_disturbance, initial_disturbance + 1);

    let inserted = target.with_conditional_mut(|runtime| {
        if runtime.ensure_pairless_cursor_obligation(cursor) {
            RuntimeNetMutation::Changed(true)
        } else {
            RuntimeNetMutation::Unchanged(false)
        }
    });
    assert!(!inserted);
    target.with(RuntimeNet::assert_cursor_obligation_invariants);
    let (duplicate_topology, duplicate_disturbance) = target.revisions();
    assert_eq!(duplicate_topology, inserted_topology);
    assert_eq!(duplicate_disturbance, inserted_disturbance);
}

#[test]
fn root_normalization_demand_is_idempotent_and_enumerable() {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    let interface = target.add_interface(Port::principal(cursor));
    let target = SharedRuntimeNet::new(target);

    let (initial_topology, initial_disturbance) = target.revisions();
    assert_eq!(
        target.poll_interface_demand(interface),
        InterfaceDemand::Cursor(cursor)
    );
    assert_eq!(
        target.with(|runtime| runtime.cursor_obligations().collect::<Vec<_>>()),
        vec![CursorObligationSnapshot {
            cursor,
            status: CursorObligationStatus::Ready,
        }]
    );
    let (ensured_topology, ensured_disturbance) = target.revisions();
    assert_eq!(ensured_topology, initial_topology + 1);
    assert_eq!(ensured_disturbance, initial_disturbance + 1);

    assert_eq!(
        target.poll_interface_demand(interface),
        InterfaceDemand::Cursor(cursor)
    );
    assert_eq!(target.revisions(), (ensured_topology, ensured_disturbance));
}

#[test]
fn interface_demand_poll_classifies_stable_roots_and_exact_work() {
    let mut disconnected = RuntimeNet::<()>::empty();
    let data = disconnected.add_node(RuntimeNode::Data(()));
    let interface = disconnected.add_interface(Port::principal(data));
    disconnected.disconnect(interface);
    let disconnected = SharedRuntimeNet::new(disconnected);
    assert_eq!(
        disconnected.poll_interface_demand(interface),
        InterfaceDemand::NormalForm
    );

    let mut data_net = RuntimeNet::<()>::empty();
    let data = data_net.add_node(RuntimeNode::Data(()));
    let data_interface = data_net.add_interface(Port::principal(data));
    let data_net = SharedRuntimeNet::new(data_net);
    assert_eq!(
        data_net.poll_interface_demand(data_interface),
        InterfaceDemand::Data
    );

    let mut bind_net = RuntimeNet::<()>::empty();
    let bind = bind_net.add_node(RuntimeNode::Bind);
    let bind_interface = bind_net.add_interface(Port::principal(bind));
    let bind_net = SharedRuntimeNet::new(bind_net);
    assert_eq!(
        bind_net.poll_interface_demand(bind_interface),
        InterfaceDemand::Bind
    );

    let mut other_net = RuntimeNet::<()>::empty();
    let erase = other_net.add_node(RuntimeNode::Erase);
    let other_interface = other_net.add_interface(Port::principal(erase));
    let other_net = SharedRuntimeNet::new(other_net);
    assert_eq!(
        other_net.poll_interface_demand(other_interface),
        InterfaceDemand::NormalForm
    );

    let mut auxiliary_net = RuntimeNet::<()>::empty();
    let bind = auxiliary_net.add_node(RuntimeNode::Bind);
    let auxiliary_interface = auxiliary_net.add_interface(Port::auxiliary(bind, 1));
    let auxiliary_net = SharedRuntimeNet::new(auxiliary_net);
    assert_eq!(
        auxiliary_net.poll_interface_demand(auxiliary_interface),
        InterfaceDemand::NormalForm
    );

    let mut active_net = RuntimeNet::<()>::empty();
    let bind = active_net.add_node(RuntimeNode::Bind);
    let erase = active_net.add_node(RuntimeNode::Erase);
    active_net.connect(Port::principal(bind), Port::principal(erase));
    let pair = ActivePairKey::new(bind, erase);
    let active_interface = active_net.add_interface(Port::auxiliary(bind, 1));
    let active_net = SharedRuntimeNet::new(active_net);
    assert_eq!(
        active_net.poll_interface_demand(active_interface),
        InterfaceDemand::ActivePair(pair)
    );
}

#[test]
fn interface_demand_poll_installs_and_recognizes_stable_cursor_owners() {
    let mut data_source = NetBuilder::<()>::new();
    let data = data_source.data(());
    let data_source = data_source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(data_source.prepare_copy_source());
    let interface = target.add_interface(Port::principal(cursor));
    let target = SharedRuntimeNet::new(target);
    let revisions = target.revisions();
    assert_eq!(
        target.poll_interface_demand(interface),
        InterfaceDemand::Cursor(cursor)
    );
    assert_eq!(target.revisions(), (revisions.0 + 1, revisions.1 + 1));
    assert_eq!(
        target.poll_interface_demand(interface),
        InterfaceDemand::Cursor(cursor)
    );
    assert_eq!(target.revisions(), (revisions.0 + 1, revisions.1 + 1));

    let mut stable_source = RuntimeNet::<()>::empty();
    let source_bind = stable_source.add_node(RuntimeNode::Bind);
    let source_exposed = stable_source.add_interface(Port::auxiliary(source_bind, 1));
    stable_source.exposed = Some(source_exposed);
    let stable_source = SharedRuntimeNet::new(stable_source);

    let mut pairless_target = RuntimeNet::empty();
    let pairless_cursor = pairless_target.begin_copy(stable_source.prepare_copy_source());
    let pairless_interface = pairless_target.add_interface(Port::principal(pairless_cursor));
    let pairless_target = SharedRuntimeNet::new(pairless_target);
    assert_eq!(
        pairless_target.poll_interface_demand(pairless_interface),
        InterfaceDemand::Cursor(pairless_cursor)
    );
    assert!(matches!(
        pairless_target.step_cursor(pairless_cursor),
        CursorStep::Stable
    ));
    assert_eq!(
        pairless_target.poll_interface_demand(pairless_interface),
        InterfaceDemand::StableCursor(pairless_cursor)
    );

    let mut pair_target = RuntimeNet::empty();
    let root_bind = pair_target.add_node(RuntimeNode::Bind);
    let pair_cursor = pair_target.begin_copy(stable_source.prepare_copy_source());
    pair_target.connect(Port::principal(root_bind), Port::principal(pair_cursor));
    let pair = ActivePairKey::new(root_bind, pair_cursor);
    let pair_interface = pair_target.add_interface(Port::auxiliary(root_bind, 1));
    let pair_target = SharedRuntimeNet::new(pair_target);
    assert_eq!(
        pair_target.poll_interface_demand(pair_interface),
        InterfaceDemand::ActivePair(pair)
    );
    assert!(matches!(
        pair_target.step_cursor(pair_cursor),
        CursorStep::Stable
    ));
    assert_eq!(
        pair_target.poll_interface_demand(pair_interface),
        InterfaceDemand::StableCursor(pair_cursor)
    );
}

#[test]
fn interface_demand_work_selection_is_revalidated_before_dispatch() {
    let active_net = source_requiring_one_reduction().instantiate_shared();
    let interface = active_net.with(|net| net.exposed());
    let pair = active_net.with(|net| net.ready_pairs()[0]);
    assert_eq!(
        active_net.poll_interface_demand(interface),
        InterfaceDemand::ActivePair(pair)
    );
    assert!(matches!(
        active_net.step_active_pair(pair),
        ActivePairStep::Reduction(_)
    ));
    assert!(matches!(
        active_net.step_active_pair(pair),
        ActivePairStep::Gone
    ));

    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    let interface = target.add_interface(Port::principal(cursor));
    let target = SharedRuntimeNet::new(target);
    assert_eq!(
        target.poll_interface_demand(interface),
        InterfaceDemand::Cursor(cursor)
    );
    assert!(matches!(
        target.step_cursor(cursor),
        CursorStep::Progressed(CursorProgress::Materialized { .. })
    ));
    assert!(matches!(target.step_cursor(cursor), CursorStep::Gone));
    assert_eq!(
        target.poll_interface_demand(interface),
        InterfaceDemand::Data
    );
}

#[test]
fn cursor_dependency_resolution_updates_both_owner_forms() {
    for fixture in [
        pairless_cursor_dependency_fixture,
        pair_owned_cursor_dependency_fixture,
    ] {
        let (runtime, cursor, expected) = fixture();
        let revisions = runtime.revisions();
        assert_eq!(
            runtime.resolve_cursor_dependency(
                cursor,
                &expected,
                CursorDependencyDisposition::Progressed,
            ),
            CursorDependencyResolution::Resolved
        );
        assert_eq!(runtime.revisions(), (revisions.0 + 1, revisions.1 + 1));
        assert!(matches!(
            runtime.with(|net| net.inspect_cursor_step(cursor)),
            CursorStepInspection::Claimable(_)
        ));

        let (runtime, cursor, expected) = fixture();
        let revisions = runtime.revisions();
        assert_eq!(
            runtime.resolve_cursor_dependency(
                cursor,
                &expected,
                CursorDependencyDisposition::Stable,
            ),
            CursorDependencyResolution::Resolved
        );
        assert_eq!(runtime.revisions(), (revisions.0 + 1, revisions.1 + 1));
        assert!(matches!(
            runtime.with(|net| net.inspect_cursor_step(cursor)),
            CursorStepInspection::Stable
        ));
    }
}

#[test]
fn cursor_dependency_resolution_rejects_stale_or_missing_parents_without_mutation() {
    let (runtime, cursor, expected) = pairless_cursor_dependency_fixture();
    let CursorDependency::LocalCursor(dependency) = expected else {
        unreachable!()
    };
    let stale = CursorDependency::LocalCursor(cursor);
    assert_ne!(stale, CursorDependency::LocalCursor(dependency));
    let revisions = runtime.revisions();
    assert_eq!(
        runtime.resolve_cursor_dependency(cursor, &stale, CursorDependencyDisposition::Progressed,),
        CursorDependencyResolution::Disturbed
    );
    assert_eq!(runtime.revisions(), revisions);
    assert_eq!(
        runtime.with(|net| net.cursor_dependency(cursor)),
        Some(CursorDependency::LocalCursor(dependency))
    );

    let missing = NodeId::from_zero_based(1_000_000);
    assert_eq!(
        runtime.resolve_cursor_dependency(missing, &stale, CursorDependencyDisposition::Stable,),
        CursorDependencyResolution::Gone
    );
    assert_eq!(runtime.revisions(), revisions);

    assert_eq!(
        runtime.resolve_cursor_dependency(
            cursor,
            &CursorDependency::LocalCursor(dependency),
            CursorDependencyDisposition::Stable,
        ),
        CursorDependencyResolution::Resolved
    );
    let stable_revisions = runtime.revisions();
    assert_eq!(
        runtime.resolve_cursor_dependency(
            cursor,
            &CursorDependency::LocalCursor(dependency),
            CursorDependencyDisposition::Progressed,
        ),
        CursorDependencyResolution::Disturbed
    );
    assert_eq!(runtime.revisions(), stable_revisions);
}

#[test]
fn cursor_steps_report_pairless_pair_owned_stable_and_contended_states() {
    let source = source_requiring_one_reduction().instantiate_shared();
    let source_pair = source.with(|runtime| runtime.ready_pairs()[0]);
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    let interface = target.add_interface(Port::principal(cursor));
    let target = SharedRuntimeNet::new(target);
    assert_eq!(
        target.poll_interface_demand(interface),
        InterfaceDemand::Cursor(cursor)
    );

    assert!(matches!(
        target.step_cursor(cursor),
        CursorStep::Dependency(CursorDependency::SourceFrontier(_))
    ));
    assert!(matches!(
        source.step_active_pair(source_pair),
        ActivePairStep::Reduction(Reduction {
            kind: ReductionKind::BindJoin,
            ..
        })
    ));
    assert!(target.with_mut(|runtime| runtime.retry_blocked_cursor(cursor)));
    assert!(matches!(
        target.step_cursor(cursor),
        CursorStep::Progressed(CursorProgress::Materialized { .. })
    ));

    let mut leaf = NetBuilder::<&'static str>::new();
    let data = leaf.data("leaf");
    let leaf = leaf.finish(data).instantiate_shared();
    let mut pair_target = RuntimeNet::empty();
    let pair_cursor = pair_target.begin_copy(leaf.prepare_copy_source());
    let local = pair_target.add_node(RuntimeNode::Data("local"));
    pair_target.connect(Port::principal(pair_cursor), Port::principal(local));
    let pair_target = SharedRuntimeNet::new(pair_target);
    assert!(matches!(
        pair_target.step_cursor(pair_cursor),
        CursorStep::Progressed(CursorProgress::Materialized { .. })
    ));

    let mut stable_source = RuntimeNet::<&'static str>::empty();
    let bind = stable_source.add_node(RuntimeNode::Bind);
    let exposed = stable_source.add_interface(Port::auxiliary(bind, 1));
    stable_source.exposed = Some(exposed);
    let mut stable_target = RuntimeNet::empty();
    let stable_source = SharedRuntimeNet::new(stable_source);
    let stable_cursor = stable_target.begin_copy(stable_source.prepare_copy_source());
    let stable_interface = stable_target.add_interface(Port::principal(stable_cursor));
    let stable_target = SharedRuntimeNet::new(stable_target);
    assert_eq!(
        stable_target.poll_interface_demand(stable_interface),
        InterfaceDemand::Cursor(stable_cursor)
    );
    assert!(matches!(
        stable_target.step_cursor(stable_cursor),
        CursorStep::Stable
    ));

    let mut claimed_source = NetBuilder::<&'static str>::new();
    let data = claimed_source.data("claimed");
    let claimed_source = claimed_source.finish(data).instantiate_shared();
    let mut claimed_target = RuntimeNet::empty();
    let claimed_cursor = claimed_target.begin_copy(claimed_source.prepare_copy_source());
    let claimed_interface = claimed_target.add_interface(Port::principal(claimed_cursor));
    let claimed_target = SharedRuntimeNet::new(claimed_target);
    assert_eq!(
        claimed_target.poll_interface_demand(claimed_interface),
        InterfaceDemand::Cursor(claimed_cursor)
    );
    assert!(
        claimed_target.with_mut(|runtime| runtime.claim_pairless_cursor_obligation(claimed_cursor))
    );
    let contention = match claimed_target.step_cursor(claimed_cursor) {
        CursorStep::Contended(contention) => contention,
        other => panic!("claimed cursor should report contention, received {other:?}"),
    };
    assert!(contention.runtime().ptr_eq(&claimed_target));
    assert_eq!(
        contention.revisions(),
        claimed_target.with_revisions(|_| ()).1
    );
    assert!(matches!(
        claimed_target.advance_claimed_cursor(claimed_cursor),
        Some(CursorProgress::Materialized { .. })
    ));
    assert!(matches!(
        claimed_target.step_cursor(claimed_cursor),
        CursorStep::Gone
    ));
}

#[test]
fn active_pair_steps_report_reduction_contention_blockage_stuck_and_gone() {
    let mut call_net = RuntimeNet::<()>::empty();
    let bind = call_net.add_node(RuntimeNode::Bind);
    let data = call_net.add_node(RuntimeNode::Data(()));
    call_net.connect(Port::principal(bind), Port::principal(data));
    let pair = ActivePairKey::new(bind, data);
    let call_net = SharedRuntimeNet::new(call_net);
    let reduction = match call_net.step_active_pair(pair) {
        ActivePairStep::Reduction(reduction) => reduction,
        other => panic!("ready call should produce a reduction, received {other:?}"),
    };
    let ReductionKind::Call { bind, data } = reduction.kind else {
        panic!("bind-data pair should produce a call");
    };
    let call = Call { pair, bind, data };
    assert!(matches!(
        call_net.step_active_pair(pair),
        ActivePairStep::Contended(_)
    ));
    call_net.with_mut(|runtime| runtime.block_claimed_call(call, 17));
    assert!(matches!(
        call_net.step_active_pair(pair),
        ActivePairStep::BlockedCall(BlockedCall { wait: 17, .. })
    ));

    let mut operator_net = RuntimeNet::<()>::empty();
    let operator = operator_net
        .add_node(RuntimeNode::Operator(TestOperator::new("blocked", |_| {
            unreachable!("blocked operator fixture is not executed")
        })));
    let data = operator_net.add_node(RuntimeNode::Data(()));
    operator_net.connect(Port::principal(operator), Port::principal(data));
    let operator_pair = ActivePairKey::new(operator, data);
    let operator_net = SharedRuntimeNet::new(operator_net);
    let call = match operator_net.step_active_pair(operator_pair) {
        ActivePairStep::Reduction(Reduction {
            kind: ReductionKind::OperatorCall { operator, data },
            ..
        }) => OperatorCall {
            pair: operator_pair,
            operator,
            data,
        },
        other => panic!("ready operator call should produce a reduction, received {other:?}"),
    };
    operator_net.with_mut(|runtime| runtime.block_claimed_operator_call(call, 23));
    assert!(matches!(
        operator_net.step_active_pair(operator_pair),
        ActivePairStep::BlockedOperatorCall(BlockedOperatorCall { wait: 23, .. })
    ));

    let mut stuck_net = RuntimeNet::<()>::empty();
    let left = stuck_net.add_node(RuntimeNode::Data(()));
    let right = stuck_net.add_node(RuntimeNode::Data(()));
    stuck_net.connect(Port::principal(left), Port::principal(right));
    let stuck_pair = ActivePairKey::new(left, right);
    let stuck_net = SharedRuntimeNet::new(stuck_net);
    assert!(matches!(
        stuck_net.step_active_pair(stuck_pair),
        ActivePairStep::Reduction(Reduction {
            kind: ReductionKind::Stuck,
            ..
        })
    ));
    assert!(matches!(
        stuck_net.step_active_pair(stuck_pair),
        ActivePairStep::Stuck(StuckPair {
            reason: StuckReason::NoRule,
            ..
        })
    ));

    let pure_net = source_requiring_one_reduction().instantiate_shared();
    let pure_pair = pure_net.with(|runtime| runtime.ready_pairs()[0]);
    assert!(matches!(
        pure_net.step_active_pair(pure_pair),
        ActivePairStep::Reduction(Reduction {
            kind: ReductionKind::BindJoin,
            ..
        })
    ));
    assert!(matches!(
        pure_net.step_active_pair(pure_pair),
        ActivePairStep::Gone
    ));

    let source = source_requiring_one_reduction().instantiate_shared();
    let cursor_net = target_waiting_on(source);
    let cursor_pair = cursor_net.active_pairs().next().unwrap();
    let cursor_net = SharedRuntimeNet::new(cursor_net);
    let cursor = match cursor_net.step_active_pair(cursor_pair) {
        ActivePairStep::Reduction(Reduction {
            kind:
                ReductionKind::RemoteCursor {
                    cursor,
                    progress: CursorProgress::Claimed,
                },
            ..
        }) => cursor,
        other => panic!("ready cursor pair should produce a claim, received {other:?}"),
    };
    assert_eq!(
        cursor_net.advance_claimed_cursor(cursor),
        Some(CursorProgress::Blocked)
    );
    assert!(matches!(
        cursor_net.step_active_pair(cursor_pair),
        ActivePairStep::Cursor(blocked) if blocked == cursor
    ));
}

#[test]
fn normalization_batch_lease_is_exclusive_and_drop_safe() {
    let mut builder = NetBuilder::<()>::new();
    let data = builder.data(());
    let runtime = builder.finish(data).instantiate_shared();

    let lease = runtime
        .try_begin_normalization_batch()
        .expect("first batch must acquire the net");
    let (first_id, contended) = runtime
        .active_normalization_batch()
        .expect("lease must be visible under the net lock");
    assert!(!contended);

    let contention = runtime
        .try_begin_normalization_batch()
        .expect_err("second batch must observe contention");
    assert!(contention.runtime().ptr_eq(&runtime));
    assert_eq!(runtime.active_normalization_batch(), Some((first_id, true)));

    lease.close();
    assert_eq!(runtime.active_normalization_batch(), None);
    let next = runtime
        .try_begin_normalization_batch()
        .expect("closed lease must release ownership");
    let (next_id, contended) = runtime.active_normalization_batch().unwrap();
    assert!(next_id > first_id);
    assert!(!contended);
    drop(next);
    assert_eq!(runtime.active_normalization_batch(), None);

    let unwind_runtime = runtime.clone();
    let unwind = std::panic::catch_unwind(move || {
        let _lease = unwind_runtime.try_begin_normalization_batch().unwrap();
        panic!("forced normalization unwind");
    });
    assert!(unwind.is_err());
    assert_eq!(runtime.active_normalization_batch(), None);
}

#[test]
fn normalization_batch_defers_disturbance_until_release() {
    let mut builder = NetBuilder::<()>::new();
    let data = builder.data(());
    let runtime = builder.finish(data).instantiate_shared();
    let initial = runtime.revisions();

    let lease = runtime.try_begin_normalization_batch().unwrap();
    runtime.with_mut(|net| net.add_node(RuntimeNode::Data(())));
    let during = runtime.revisions();
    assert!(during.0 > initial.0, "topology must remain authoritative");
    assert_eq!(
        during.1, initial.1,
        "batch mutations must not wake followers early"
    );

    let contention = runtime
        .try_begin_normalization_batch()
        .expect_err("follower must register against the active batch");
    assert_eq!(contention.revisions().disturbance_epoch(), initial.1);
    lease.close();
    let released = runtime.revisions();
    assert_eq!(released.0, during.0);
    assert_eq!(released.1, initial.1 + 1);

    let clean = runtime.try_begin_normalization_batch().unwrap();
    clean.close();
    assert_eq!(runtime.revisions(), released);
}

#[test]
fn normalization_batch_wakes_a_registered_follower_once_at_release() {
    let mut builder = NetBuilder::<()>::new();
    let data = builder.data(());
    let runtime = builder.finish(data).instantiate_shared();
    let lease = runtime.try_begin_normalization_batch().unwrap();
    let follower_runtime = runtime.clone();
    let (registered_tx, registered_rx) = mpsc::channel();
    let (woke_tx, woke_rx) = mpsc::channel();
    let follower = thread::spawn(move || {
        let contention = follower_runtime
            .try_begin_normalization_batch()
            .expect_err("leader must still own the batch");
        registered_tx.send(()).unwrap();
        contention
            .runtime()
            .wait_for_disturbance(contention.revisions().disturbance_epoch());
        woke_tx.send(()).unwrap();
    });

    registered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    for _ in 0..8 {
        runtime.with_mut(|net| net.add_node(RuntimeNode::Data(())));
        assert!(woke_rx.try_recv().is_err());
    }
    lease.close();
    woke_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    follower.join().unwrap();
}

#[test]
fn pairless_cursor_obligation_transitions_have_one_owner() {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let blocked_cursor = target.begin_copy(source.prepare_copy_source());
    let stable_cursor = target.begin_copy(source.prepare_copy_source());

    assert!(target.ensure_pairless_cursor_obligation(blocked_cursor));
    assert_eq!(
        target.cursor_claim_owner(blocked_cursor),
        Some(CursorClaimOwner::Obligation)
    );
    assert!(!target.has_in_flight_claims());
    assert!(target.claim_pairless_cursor_obligation(blocked_cursor));
    assert!(!target.claim_pairless_cursor_obligation(blocked_cursor));
    assert!(target.has_in_flight_claims());
    assert!(target.block_pairless_cursor_obligation(
        blocked_cursor,
        CursorDependency::LocalCursor(stable_cursor),
    ));
    assert!(!target.has_in_flight_claims());
    assert!(matches!(
        &target
            .cursor_obligations
            .get(&blocked_cursor)
            .expect("blocked obligation should remain installed")
            .state,
        PairlessCursorState::Blocked(CursorDependency::LocalCursor(cursor))
            if *cursor == stable_cursor
    ));
    assert!(!target.stabilize_pairless_cursor_obligation(blocked_cursor));

    assert!(target.ensure_pairless_cursor_obligation(stable_cursor));
    assert!(target.claim_pairless_cursor_obligation(stable_cursor));
    assert!(target.stabilize_pairless_cursor_obligation(stable_cursor));
    assert!(matches!(
        target
            .cursor_obligations
            .get(&stable_cursor)
            .expect("stable obligation should remain installed")
            .state,
        PairlessCursorState::Stable
    ));
    target.assert_cursor_obligation_invariants();
}

#[test]
fn removing_a_cursor_removes_its_dormant_obligation() {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    assert!(target.ensure_pairless_cursor_obligation(cursor));

    assert!(matches!(
        target.remove_node(cursor),
        RuntimeNode::RemoteCursor { .. }
    ));
    assert!(!target.cursor_obligations.contains_key(&cursor));
    assert_eq!(target.cursor_claim_owner(cursor), None);
    target.assert_cursor_obligation_invariants();
}

#[test]
fn active_pair_cursor_owner_is_distinct_from_pairless_obligation_owner() {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    let bind = target.add_node(RuntimeNode::Bind);
    target.connect(Port::principal(cursor), Port::principal(bind));
    let pair = ActivePairKey::new(cursor, bind);

    assert_eq!(
        target.cursor_claim_owner(cursor),
        Some(CursorClaimOwner::ActivePair(pair))
    );
    assert!(target.cursor_obligations.is_empty());
}

#[test]
fn connecting_a_cursor_transfers_ready_blocked_and_stable_obligations_to_the_pair() {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();

    let mut ready_target = RuntimeNet::empty();
    let ready_cursor = ready_target.begin_copy(source.prepare_copy_source());
    let ready_bind = ready_target.add_node(RuntimeNode::Bind);
    assert!(ready_target.ensure_pairless_cursor_obligation(ready_cursor));
    ready_target.connect(Port::principal(ready_cursor), Port::principal(ready_bind));
    let ready_pair = ActivePairKey::new(ready_cursor, ready_bind);
    assert!(ready_target.cursor_obligations.is_empty());
    assert!(matches!(
        ready_target.active.get(&ready_pair),
        Some(ActivePairState::Ready)
    ));

    let mut blocked_target = RuntimeNet::empty();
    let blocked_cursor = blocked_target.begin_copy(source.prepare_copy_source());
    let dependency = blocked_target.begin_copy(source.prepare_copy_source());
    let blocked_bind = blocked_target.add_node(RuntimeNode::Bind);
    assert!(blocked_target.ensure_pairless_cursor_obligation(blocked_cursor));
    assert!(blocked_target.claim_pairless_cursor_obligation(blocked_cursor));
    assert!(blocked_target.block_pairless_cursor_obligation(
        blocked_cursor,
        CursorDependency::LocalCursor(dependency),
    ));
    blocked_target.connect(
        Port::principal(blocked_cursor),
        Port::principal(blocked_bind),
    );
    let blocked_pair = ActivePairKey::new(blocked_cursor, blocked_bind);
    assert!(
        !blocked_target
            .cursor_obligations
            .contains_key(&blocked_cursor)
    );
    assert!(matches!(
        blocked_target.active.get(&blocked_pair),
        Some(ActivePairState::BlockedCursor {
            cursor,
            blockage: CursorBlockage::Dependency(CursorDependency::LocalCursor(waiting_on)),
        }) if *cursor == blocked_cursor && *waiting_on == dependency
    ));
    assert!(matches!(
        blocked_target.cursor_dependency(blocked_cursor),
        Some(CursorDependency::LocalCursor(waiting_on)) if waiting_on == dependency
    ));

    let mut stable_target = RuntimeNet::empty();
    let stable_cursor = stable_target.begin_copy(source.prepare_copy_source());
    let stable_bind = stable_target.add_node(RuntimeNode::Bind);
    assert!(stable_target.ensure_pairless_cursor_obligation(stable_cursor));
    assert!(stable_target.claim_pairless_cursor_obligation(stable_cursor));
    assert!(stable_target.stabilize_pairless_cursor_obligation(stable_cursor));
    stable_target.connect(Port::principal(stable_cursor), Port::principal(stable_bind));
    let stable_pair = ActivePairKey::new(stable_cursor, stable_bind);
    assert!(
        !stable_target
            .cursor_obligations
            .contains_key(&stable_cursor)
    );
    assert!(matches!(
        stable_target.active.get(&stable_pair),
        Some(ActivePairState::BlockedCursor {
            cursor,
            blockage: CursorBlockage::Stable,
        }) if *cursor == stable_cursor
    ));
    assert_eq!(
        stable_target.cursor_claim_owner(stable_cursor),
        Some(CursorClaimOwner::ActivePair(stable_pair))
    );
}

#[test]
#[should_panic(expected = "in-flight pairless cursor claim cannot change graph ownership")]
fn connecting_a_cursor_rejects_transfer_of_a_claimed_obligation() {
    let mut source = NetBuilder::<()>::new();
    let data = source.data(());
    let source = source.finish(data).instantiate_shared();
    let mut target = RuntimeNet::empty();
    let cursor = target.begin_copy(source.prepare_copy_source());
    let bind = target.add_node(RuntimeNode::Bind);
    assert!(target.ensure_pairless_cursor_obligation(cursor));
    assert!(target.claim_pairless_cursor_obligation(cursor));

    target.connect(Port::principal(cursor), Port::principal(bind));
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

    assert!(net.restore_blocked_call(call, 17));
    assert_eq!(net.blocked_call(pair), Some(BlockedCall { pair, wait: 17 }));
    assert!(net.retry_blocked_call(call, &17));
    assert!(net.release_claimed_call(call));
    assert!(matches!(
        net.reduce_pair(pair),
        Some(Reduction {
            kind: ReductionKind::Call { .. },
            ..
        })
    ));
}

#[test]
fn call_release_and_restore_reject_non_claimed_pairs_without_mutation() {
    let mut net = RuntimeNet::<()>::empty();
    let bind = net.add_node(RuntimeNode::Bind);
    let data = net.add_node(RuntimeNode::Data(()));
    net.connect(Port::principal(bind), Port::principal(data));
    let pair = ActivePairKey::new(bind, data);
    let call = Call { pair, bind, data };

    assert!(!net.release_claimed_call(call));
    assert!(!net.restore_blocked_call(call, 17));
    assert!(matches!(
        net.reduce_pair(pair),
        Some(Reduction {
            kind: ReductionKind::Call { .. },
            ..
        })
    ));
    net.block_claimed_call(call, 17);
    assert!(!net.release_claimed_call(call));
    assert!(!net.restore_blocked_call(call, 18));
    assert_eq!(net.blocked_call(pair), Some(BlockedCall { pair, wait: 17 }));
}

#[test]
fn claimed_call_reads_are_quiet_while_block_and_failure_publish() {
    let claim = |data| {
        let mut net = RuntimeNet::<i32>::empty();
        let bind = net.add_node(RuntimeNode::Bind);
        let data_node = net.add_node(RuntimeNode::Data(data));
        net.connect(Port::principal(bind), Port::principal(data_node));
        let net = SharedRuntimeNet::new(net);
        let reduction = net
            .with_optional_mut(RuntimeNet::reduce_next)
            .expect("bind-data must claim a call");
        let ReductionKind::Call { bind, data } = reduction.kind else {
            panic!("expected a claimed call")
        };
        (
            net,
            Call {
                pair: reduction.pair,
                bind,
                data,
            },
        )
    };

    let (blocked, blocked_call) = claim(7);
    let before_read = blocked.with_revisions(|_| ()).1;
    assert_eq!(blocked.with(|net| net.claim_call(blocked_call)), Some(7));
    assert_eq!(blocked.with_revisions(|_| ()).1, before_read);
    blocked.with_mut(|net| net.block_claimed_call(blocked_call, 17));
    let after_block = blocked.with_revisions(|_| ()).1;
    assert_eq!(
        after_block.topology_revision(),
        before_read.topology_revision() + 1
    );
    assert_eq!(
        after_block.disturbance_epoch(),
        before_read.disturbance_epoch() + 1
    );

    let (failed, failed_call) = claim(11);
    let before_failure = failed.with_revisions(|_| ()).1;
    assert_eq!(failed.with(|net| net.claim_call(failed_call)), Some(11));
    assert_eq!(failed.with_revisions(|_| ()).1, before_failure);
    failed.with_mut(|net| net.fail_claimed_call(failed_call, Arc::from("not callable")));
    let after_failure = failed.with_revisions(|_| ()).1;
    assert_eq!(
        after_failure.topology_revision(),
        before_failure.topology_revision() + 1
    );
    assert_eq!(
        after_failure.disturbance_epoch(),
        before_failure.disturbance_epoch() + 1
    );
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
fn nested_cursor_preserves_structured_failure_across_unrelated_source_progress() {
    let mut source = RuntimeNet::<StructuredSpecialization>::empty();
    let failed_bind = source.add_node(RuntimeNode::Bind);
    let failed_data = source.add_node(RuntimeNode::Data(7));
    source.connect(Port::principal(failed_bind), Port::principal(failed_data));
    let failed_result = source.add_node(RuntimeNode::Data(0));
    source.connect(
        Port::auxiliary(failed_bind, 2),
        Port::principal(failed_result),
    );
    let exposed = source.add_interface(Port::auxiliary(failed_bind, 1));
    source.exposed = Some(exposed);
    let failed_pair = ActivePairKey::new(failed_bind, failed_data);

    let unrelated_left = source.add_node(RuntimeNode::Bind);
    let unrelated_right = source.add_node(RuntimeNode::Bind);
    source.connect(
        Port::principal(unrelated_left),
        Port::principal(unrelated_right),
    );
    for index in 1..=2 {
        let left_data = source.add_node(RuntimeNode::Data(index as i32));
        let right_data = source.add_node(RuntimeNode::Data(-(index as i32)));
        source.connect(
            Port::auxiliary(unrelated_left, index),
            Port::principal(left_data),
        );
        source.connect(
            Port::auxiliary(unrelated_right, index),
            Port::principal(right_data),
        );
    }
    let unrelated_pair = ActivePairKey::new(unrelated_left, unrelated_right);

    let reduction = source
        .reduce_pair(failed_pair)
        .expect("failed call should be claimable");
    let ReductionKind::Call { bind, data } = reduction.kind else {
        panic!("failed source pair should be a call");
    };
    let call = Call {
        pair: failed_pair,
        bind,
        data,
    };
    let error = StructuredStuckReason {
        code: 42,
        detail: Arc::from("nested structured failure"),
    };
    source.fail_claimed_call(call, error.clone());
    let source = SharedRuntimeNet::new(source);

    let mut target = RuntimeNet::<StructuredSpecialization>::empty();
    let local = target.add_node(RuntimeNode::Data(0));
    let cursor = target.begin_copy(source.prepare_copy_source());
    target.connect(Port::principal(local), Port::principal(cursor));
    assert_eq!(reduce_next_cursor(&mut target).1, CursorProgress::Blocked);
    let observation = match target
        .cursor_dependency(cursor)
        .expect("outer cursor should retain its nested endpoint")
    {
        CursorDependency::SourceFrontier(observation) => {
            assert!(observation.source().ptr_eq(&source));
            assert_eq!(
                observation.endpoint(),
                DemandEndpoint::ActivePair(failed_pair)
            );
            observation
        }
        dependency => panic!("expected a nested source pair, got {dependency:?}"),
    };

    let progress_barrier = Arc::new(Barrier::new(2));
    let worker_barrier = progress_barrier.clone();
    let worker_source = source.clone();
    let worker = thread::spawn(move || {
        worker_barrier.wait();
        assert!(matches!(
            worker_source.with_mut(|runtime| runtime.reduce_pair(unrelated_pair)),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));
        worker_barrier.wait();
    });
    progress_barrier.wait();
    progress_barrier.wait();
    worker
        .join()
        .expect("unrelated source worker should not panic");

    let propagated = match observation.step_active_pair(failed_pair) {
        ActivePairStep::Stuck(stuck) => stuck,
        step => panic!("permanent nested failure should survive disturbance, got {step:?}"),
    };
    assert_eq!(
        propagated,
        StuckPair {
            pair: failed_pair,
            reason: StuckReason::Specialization(error),
        },
        "unrelated source versions must not postpone the terminal failure"
    );
}

#[test]
fn claimed_callable_data_splices_directly_to_its_operator() {
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

#[test]
fn claimed_callable_splice_preserves_non_data_neighbors() {
    let mut net = RuntimeNet::<i32>::empty();
    let application = net.add_node(RuntimeNode::Bind);
    let callable = net.add_node(RuntimeNode::Data(0));
    let argument_interface = net.add_node(RuntimeNode::Interface);
    let result_interface = net.add_node(RuntimeNode::Interface);
    let argument = Port::auxiliary(argument_interface, 1);
    let result = Port::auxiliary(result_interface, 1);
    net.connect(Port::principal(application), Port::principal(callable));
    net.connect(Port::auxiliary(application, 1), argument);
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

    net.resume_claimed_call_with_operator(
        call,
        TestOperator::new("identity", |value| Ok(OperatorYield::Data(*value))),
    );

    assert!(net.node(application).is_none());
    assert!(net.node(callable).is_none());
    let operator_principal = net
        .interface_neighbor(argument)
        .expect("argument neighbor should remain connected");
    assert!(operator_principal.is_principal());
    assert!(matches!(
        net.node(operator_principal.node()),
        Some(RuntimeNode::Operator(_))
    ));
    assert_eq!(
        net.interface_neighbor(result),
        Some(Port::auxiliary(operator_principal.node(), 1))
    );
    assert!(net.reduce_next().is_none());
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
fn blocked_operator_call_requires_its_current_wait_token_to_be_reclaimed() {
    let (mut net, call, _) = operator_call_net(
        TestOperator::new("blocked", |value| Ok(OperatorYield::Data(*value))),
        42,
    );

    net.block_claimed_operator_call(call, 17);

    assert_eq!(
        net.blocked_operator_call(call.pair),
        Some(BlockedOperatorCall {
            pair: call.pair,
            wait: 17,
        })
    );
    assert!(!net.retry_blocked_operator_call(call, &16));
    assert_eq!(net.blocked_operator_call(call.pair).unwrap().wait, 17);
    assert!(net.retry_blocked_operator_call(call, &17));
    let (operator, data) = net.operator_call_parts(call);
    assert_eq!(operator.apply(&data).unwrap(), OperatorYield::Data(42));
    assert!(net.principals_connect(call.pair));

    assert!(net.restore_blocked_operator_call(call, 17));
    assert_eq!(
        net.blocked_operator_call(call.pair),
        Some(BlockedOperatorCall {
            pair: call.pair,
            wait: 17,
        })
    );
    assert!(net.retry_blocked_operator_call(call, &17));
    assert!(net.release_claimed_operator_call(call));
    assert!(matches!(
        net.reduce_pair(call.pair),
        Some(Reduction {
            kind: ReductionKind::OperatorCall { .. },
            ..
        })
    ));
}

#[test]
fn operator_release_and_restore_reject_non_claimed_pairs_without_mutation() {
    let (mut net, call, _) = operator_call_net(
        TestOperator::new("identity", |value| Ok(OperatorYield::Data(*value))),
        42,
    );
    assert!(net.release_claimed_operator_call(call));
    assert!(!net.release_claimed_operator_call(call));
    assert!(!net.restore_blocked_operator_call(call, 17));
    assert!(matches!(
        net.reduce_pair(call.pair),
        Some(Reduction {
            kind: ReductionKind::OperatorCall { .. },
            ..
        })
    ));
    net.block_claimed_operator_call(call, 17);
    assert!(!net.release_claimed_operator_call(call));
    assert!(!net.restore_blocked_operator_call(call, 18));
    assert_eq!(
        net.blocked_operator_call(call.pair),
        Some(BlockedOperatorCall {
            pair: call.pair,
            wait: 17,
        })
    );
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
    let cursor = target.begin_copy(source.prepare_copy_source());
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
fn source_change_between_cursor_inspection_publication_and_wait_is_not_lost() {
    let source = source_requiring_one_reduction().instantiate_shared();
    let source_pair = source.with(|runtime| runtime.ready_pairs()[0]);
    let mut target = target_waiting_on(source.clone());
    let Some(Reduction {
        kind:
            ReductionKind::RemoteCursor {
                cursor,
                progress: CursorProgress::Claimed,
            },
        ..
    }) = target.reduce_next()
    else {
        panic!("target should claim its remote cursor");
    };
    let claim = target
        .cursor_claim(cursor)
        .expect("claimed cursor should remain inspectable");
    let frontier = source.inspect_source_frontier(claim.remote);
    let observation = frontier
        .observation
        .as_ref()
        .expect("the inspected pair should have a versioned observation");
    let observed_topology = observation.observed_topology;
    let observed_revisions = source.with_revisions(|_| ()).1;
    assert_eq!(observed_revisions.topology_revision(), observed_topology);
    let inspected_pair = match &frontier.shape {
        SourceFrontierShape::StableAuxiliary {
            terminal_pair: Some(pair),
            ..
        } => pair.to_owned(),
        SourceFrontierShape::ActiveAuxiliary { entered, partner } => {
            ActivePairKey::new(entered.node(), partner.node())
        }
        _ => panic!("source cursor should inspect the pending source pair"),
    };
    assert_eq!(inspected_pair, source_pair);

    let mutation_barrier = Arc::new(Barrier::new(2));
    let worker_barrier = mutation_barrier.clone();
    let worker_source = source.clone();
    let mutator = thread::spawn(move || {
        worker_barrier.wait();
        assert!(matches!(
            worker_source.with_mut(|runtime| runtime.reduce_pair(source_pair)),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));
        worker_barrier.wait();
    });
    mutation_barrier.wait();
    mutation_barrier.wait();
    mutator.join().expect("source mutator should not panic");
    assert!(!source.with(|runtime| runtime.contains_active_pair(source_pair)));
    assert_ne!(
        source.with_revisions(|_| ()).1.topology_revision(),
        observed_topology
    );
    assert!(matches!(
        frontier
            .observation
            .as_ref()
            .expect("pair observation should remain attached")
            .step_active_pair(source_pair),
        ActivePairStep::Disturbed
    ));

    assert_eq!(
        target.finish_cursor_claim(claim, frontier),
        CursorProgress::Blocked,
        "the forced ordering publishes the now-stale inspected endpoint"
    );
    assert!(matches!(
        target.cursor_dependency(cursor),
        Some(CursorDependency::SourceFrontier(observation))
            if observation.endpoint() == DemandEndpoint::ActivePair(source_pair)
    ));

    let waiter_source = source.clone();
    let (sender, receiver) = mpsc::channel();
    let waiter = thread::spawn(move || {
        waiter_source.wait_for_disturbance(observed_revisions.disturbance_epoch());
        sender.send(()).expect("test receiver should remain open");
    });
    if receiver.recv_timeout(Duration::from_secs(2)).is_err() {
        // Release a buggy waiter before failing so the test cannot leave a
        // detached blocked thread behind.
        source.with_mut(|_| {});
        waiter.join().expect("runtime waiter should not panic");
        panic!("a source change preceding wait registration must remain observable");
    }
    waiter.join().expect("runtime waiter should not panic");

    assert!(target.retry_blocked_cursor(cursor));
    assert!(matches!(
        reduce_next_cursor(&mut target).1,
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
    let CursorDependency::SourceFrontier(observation) = dependency else {
        panic!("active source call should remain an exact source dependency");
    };
    assert!(observation.source().ptr_eq(&source));
    assert_eq!(observation.endpoint(), DemandEndpoint::ActivePair(pair));
    source.with(|source| {
        assert_eq!(source.active.get(&pair), Some(&ActivePairState::Claimed));
    });
    let revisions = source.with_revisions(|_| ()).1;
    assert!(matches!(
        observation.step_active_pair(pair),
        ActivePairStep::Contended(_)
    ));
    assert_eq!(
        source.with_revisions(|_| ()).1,
        revisions,
        "an unavailable observed claim must not publish a false disturbance"
    );
    assert!(
        !target
            .nodes
            .values()
            .any(|entry| matches!(entry.node, RuntimeNode::Bind))
    );

    let call = source
        .with(|runtime| runtime.call(pair))
        .expect("claimed source call should remain structurally available");
    let error: Arc<str> = Arc::from("finished after observation test");
    source.with_mut(|runtime| {
        runtime.fail_claimed_call(call, error.clone());
    });
    let ActivePairStep::Stuck(stuck) = observation.step_active_pair(pair) else {
        panic!("an exact claimed dependency should propagate its terminal failure");
    };
    assert_eq!(
        stuck,
        StuckPair {
            pair,
            reason: StuckReason::Specialization(error),
        }
    );
}

#[test]
fn layered_cursor_reports_and_follows_an_exact_dependency() {
    let mut leaf = NetBuilder::new();
    let data = leaf.data("leaf");
    let leaf = leaf.finish(data).instantiate_shared();

    let mut middle = RuntimeNet::empty();
    let middle_cursor = middle.begin_copy(leaf.prepare_copy_source());
    let exposed = middle.add_interface(Port::principal(middle_cursor));
    middle.exposed = Some(exposed);
    let middle = SharedRuntimeNet::new(middle);

    let mut outer = target_waiting_on(middle.clone());
    let (outer_cursor, progress) = reduce_next_cursor(&mut outer);
    assert_eq!(progress, CursorProgress::Blocked);
    let dependency = outer
        .cursor_dependency(outer_cursor)
        .expect("layered cursor should retain an exact dependency");
    let CursorDependency::SourceCursor(observation) = dependency else {
        panic!("layered cursor should point to its exact source cursor");
    };
    assert!(observation.source().ptr_eq(&middle));
    assert_eq!(
        observation.endpoint(),
        DemandEndpoint::Cursor(middle_cursor)
    );
    assert!(matches!(
        observation.step_cursor(middle_cursor),
        CursorStep::Progressed(CursorProgress::Materialized { .. })
    ));
    assert!(matches!(
        observation.step_cursor(middle_cursor),
        CursorStep::Disturbed
    ));
    assert!(outer.retry_blocked_cursor(outer_cursor));
    assert!(matches!(
        reduce_next_cursor(&mut outer).1,
        CursorProgress::Materialized { .. }
    ));
}

#[test]
fn nested_cursor_demand_reuses_a_claimed_source_obligation() {
    let mut leaf = NetBuilder::new();
    let data = leaf.data("leaf");
    let leaf = leaf.finish(data).instantiate_shared();

    let mut middle = RuntimeNet::empty();
    let middle_cursor = middle.begin_copy(leaf.prepare_copy_source());
    let exposed = middle.add_interface(Port::principal(middle_cursor));
    middle.exposed = Some(exposed);
    let middle = SharedRuntimeNet::new(middle);

    let mut first = target_waiting_on(middle.clone());
    let (first_cursor, first_progress) = reduce_next_cursor(&mut first);
    assert_eq!(first_progress, CursorProgress::Blocked);
    let Some(CursorDependency::SourceCursor(first_observation)) =
        first.cursor_dependency(first_cursor)
    else {
        panic!("first outer cursor should observe the middle cursor");
    };
    assert_eq!(
        first_observation.endpoint(),
        DemandEndpoint::Cursor(middle_cursor)
    );
    assert_eq!(
        middle.with_mut(|runtime| claim_test_cursor(runtime, middle_cursor)),
        Some(CursorProgress::Claimed)
    );
    assert!(matches!(
        first_observation.step_cursor(middle_cursor),
        CursorStep::Disturbed
    ));

    let mut second = target_waiting_on(middle.clone());
    let (second_cursor, second_progress) = reduce_next_cursor(&mut second);
    assert_eq!(second_progress, CursorProgress::Blocked);
    let Some(CursorDependency::SourceCursor(second_observation)) =
        second.cursor_dependency(second_cursor)
    else {
        panic!("second outer cursor should observe the middle cursor");
    };
    assert!(matches!(
        second_observation.step_cursor(middle_cursor),
        CursorStep::Contended(_)
    ));

    assert!(matches!(
        middle.advance_claimed_cursor(middle_cursor),
        Some(CursorProgress::Materialized { .. })
    ));
    assert!(matches!(
        second_observation.step_cursor(middle_cursor),
        CursorStep::Disturbed
    ));
}

#[test]
fn root_cursor_claim_remains_exclusive_while_source_inspection_is_in_flight() {
    let mut source = NetBuilder::<&'static str>::new();
    let data = source.data("value");
    let source = source.finish(data).instantiate_shared();

    let mut target = RuntimeNet::empty();
    let root_cursor = target.begin_copy(source.prepare_copy_source());
    let exposed = target.add_interface(Port::principal(root_cursor));

    assert_eq!(
        claim_test_interface_cursor(&mut target, exposed),
        Some(CursorProgress::Claimed)
    );
    assert!(matches!(
        target
            .cursor_obligations
            .get(&root_cursor)
            .map(|obligation| &obligation.state),
        Some(PairlessCursorState::Claimed)
    ));
    assert!(
        target.has_in_flight_claims(),
        "pairless root work must remain visible to quiescence detection"
    );
    assert_eq!(
        claim_test_interface_cursor(&mut target, exposed),
        None,
        "a second evaluator must observe the in-flight root cursor claim"
    );
    assert!(matches!(
        finish_claimed_cursor(&mut target, root_cursor),
        CursorProgress::Materialized { .. }
    ));
    assert!(!target.has_in_flight_claims());
    assert!(!target.cursor_obligations.contains_key(&root_cursor));
    assert_eq!(target.interface_data(exposed), Some(&"value"));
}

#[test]
fn pairless_cursor_claim_publishes_blocked_and_stable_obligations() {
    let source = source_requiring_one_reduction().instantiate_shared();
    let mut blocked_target = RuntimeNet::empty();
    let blocked_cursor = blocked_target.begin_copy(source.prepare_copy_source());
    let blocked_interface = blocked_target.add_interface(Port::principal(blocked_cursor));
    assert_eq!(
        claim_test_interface_cursor(&mut blocked_target, blocked_interface),
        Some(CursorProgress::Claimed)
    );
    assert_eq!(
        finish_claimed_cursor(&mut blocked_target, blocked_cursor),
        CursorProgress::Blocked
    );
    assert!(matches!(
        blocked_target
            .cursor_obligations
            .get(&blocked_cursor)
            .map(|obligation| &obligation.state),
        Some(PairlessCursorState::Blocked(
            CursorDependency::SourceFrontier(_)
        ))
    ));

    let mut stable_source = RuntimeNet::<&'static str>::empty();
    let bind = stable_source.add_node(RuntimeNode::Bind);
    let exposed = stable_source.add_interface(Port::auxiliary(bind, 1));
    stable_source.exposed = Some(exposed);
    let stable_source = SharedRuntimeNet::new(stable_source);
    let mut stable_target = RuntimeNet::empty();
    let stable_cursor = stable_target.begin_copy(stable_source.prepare_copy_source());
    let stable_interface = stable_target.add_interface(Port::principal(stable_cursor));
    assert_eq!(
        claim_test_interface_cursor(&mut stable_target, stable_interface),
        Some(CursorProgress::Claimed)
    );
    assert_eq!(
        finish_claimed_cursor(&mut stable_target, stable_cursor),
        CursorProgress::Blocked
    );
    assert!(matches!(
        stable_target
            .cursor_obligations
            .get(&stable_cursor)
            .map(|obligation| &obligation.state),
        Some(PairlessCursorState::Stable)
    ));
    assert_eq!(
        claim_test_interface_cursor(&mut stable_target, stable_interface),
        None
    );
}

#[test]
fn pair_owned_cursor_retains_stable_blockage_without_a_dependency() {
    let mut source = RuntimeNet::<&'static str>::empty();
    let bind = source.add_node(RuntimeNode::Bind);
    let exposed = source.add_interface(Port::auxiliary(bind, 1));
    source.exposed = Some(exposed);

    let mut target = target_waiting_on(SharedRuntimeNet::new(source));
    let (cursor, progress) = reduce_next_cursor(&mut target);
    assert_eq!(progress, CursorProgress::Blocked);
    let pair = target
        .active_pair_key(cursor)
        .expect("pair-owned cursor should retain its active pair");
    assert!(matches!(
        target.active.get(&pair),
        Some(ActivePairState::BlockedCursor {
            cursor: blocked,
            blockage: CursorBlockage::Stable,
        }) if *blocked == cursor
    ));
    assert_eq!(target.cursor_dependency(cursor), None);
    assert!(!target.retry_blocked_cursor(cursor));
}

#[test]
fn concurrent_interface_demands_share_one_pairless_cursor_claim() {
    let mut source = NetBuilder::<&'static str>::new();
    let data = source.data("value");
    let source = source.finish(data).instantiate_shared();

    let mut target = RuntimeNet::empty();
    let root_cursor = target.begin_copy(source.prepare_copy_source());
    let exposed = target.add_interface(Port::principal(root_cursor));
    let target = SharedRuntimeNet::new(target);
    let claimed = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));

    let worker_target = target.clone();
    let worker_claimed = claimed.clone();
    let worker_release = release.clone();
    let worker = thread::spawn(move || {
        assert_eq!(
            worker_target.with_mut(|runtime| claim_test_interface_cursor(runtime, exposed)),
            Some(CursorProgress::Claimed)
        );
        worker_claimed.wait();
        worker_release.wait();
        assert!(matches!(
            worker_target.advance_claimed_cursor(root_cursor),
            Some(CursorProgress::Materialized { .. })
        ));
    });

    claimed.wait();
    assert_eq!(
        target.with_mut(|runtime| claim_test_interface_cursor(runtime, exposed)),
        None,
        "a concurrent demand must not duplicate the pairless cursor claim"
    );
    assert!(target.with(|runtime| runtime.has_in_flight_claims()));
    release.wait();
    worker.join().expect("cursor worker should not panic");

    assert_eq!(
        target.with(|runtime| runtime.interface_data(exposed).copied()),
        Some("value")
    );
    assert!(!target.with(|runtime| runtime.has_in_flight_claims()));
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
    let root_cursor = target.begin_copy(source.prepare_copy_source());
    let target_exposed = target.add_interface(Port::principal(root_cursor));
    assert_eq!(
        claim_test_interface_cursor(&mut target, target_exposed),
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
        claim_test_cursor(&mut target, result_cursor),
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
        claim_test_cursor(&mut target, argument_cursor),
        Some(CursorProgress::Claimed)
    );
    assert!(matches!(
        finish_claimed_cursor(&mut target, argument_cursor),
        CursorProgress::Materialized { .. }
    ));
    assert_eq!(
        claim_test_cursor(&mut target, result_cursor),
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
    let root_cursor = target.begin_copy(source.prepare_copy_source());
    let target_exposed = target.add_interface(Port::principal(root_cursor));
    assert_eq!(
        claim_test_interface_cursor(&mut target, target_exposed),
        Some(CursorProgress::Claimed)
    );
    assert!(matches!(
        finish_claimed_cursor(&mut target, root_cursor),
        CursorProgress::Materialized { .. }
    ));

    let cursor = target.copies.values().next().unwrap().frontiers[&Port::auxiliary(root, 2)];
    assert_eq!(
        claim_test_cursor(&mut target, cursor),
        Some(CursorProgress::Claimed)
    );
    assert_eq!(
        finish_claimed_cursor(&mut target, cursor),
        CursorProgress::Blocked
    );
    assert!(matches!(
        target.cursor_dependency(cursor),
        Some(CursorDependency::SourceFrontier(observation))
            if observation.source().ptr_eq(&source)
                && observation.endpoint() == DemandEndpoint::ActivePair(pair)
    ));
}

#[test]
fn auxiliary_cursor_recomputes_its_spine_after_each_terminal_pair() {
    let mut source: RuntimeNet<&'static str> = RuntimeNet::empty();
    let first = source.add_node(RuntimeNode::Bind);
    let second = source.add_node(RuntimeNode::Bind);
    let terminal_left = source.add_node(RuntimeNode::Bind);
    let terminal_right = source.add_node(RuntimeNode::Bind);
    let next = source.add_node(RuntimeNode::Bind);
    let last = source.add_node(RuntimeNode::Bind);
    let result = source.add_node(RuntimeNode::Data("result"));
    let exposed = source.add_interface(Port::auxiliary(first, 1));
    source.exposed = Some(exposed);

    source.connect(Port::principal(first), Port::auxiliary(second, 1));
    source.connect(Port::principal(second), Port::auxiliary(terminal_left, 1));
    source.connect(
        Port::principal(terminal_left),
        Port::principal(terminal_right),
    );
    source.connect(Port::auxiliary(terminal_right, 1), Port::principal(next));
    source.connect(Port::auxiliary(next, 1), Port::principal(last));
    source.connect(Port::auxiliary(last, 1), Port::principal(result));

    for (left, right) in [
        (terminal_left, terminal_right),
        (second, next),
        (first, last),
    ] {
        let left_data = source.add_node(RuntimeNode::Data("unused-left"));
        let right_data = source.add_node(RuntimeNode::Data("unused-right"));
        source.connect(Port::auxiliary(left, 2), Port::principal(left_data));
        source.connect(Port::auxiliary(right, 2), Port::principal(right_data));
    }

    let terminal_pair = ActivePairKey::new(terminal_left, terminal_right);
    let second_pair = ActivePairKey::new(second, next);
    let first_pair = ActivePairKey::new(first, last);
    let source = SharedRuntimeNet::new(source);
    let mut target = target_waiting_on(source.clone());

    let (cursor, progress) = reduce_next_cursor(&mut target);
    assert_eq!(progress, CursorProgress::Blocked);
    assert!(matches!(
        target.cursor_dependency(cursor),
        Some(CursorDependency::SourceFrontier(observation))
            if observation.endpoint() == DemandEndpoint::ActivePair(terminal_pair)
    ));

    for (consumed, next_dependency) in [
        (terminal_pair, Some(second_pair)),
        (second_pair, Some(first_pair)),
        (first_pair, None),
    ] {
        let observation = match target
            .cursor_dependency(cursor)
            .expect("blocked cursor should retain its frontier observation")
        {
            CursorDependency::SourceFrontier(observation) => observation,
            dependency => panic!("expected a source-frontier observation, got {dependency:?}"),
        };
        assert_eq!(observation.endpoint(), DemandEndpoint::ActivePair(consumed));
        let observed_topology = observation.observed_topology;
        assert_eq!(
            source.with_revisions(|_| ()).1.topology_revision(),
            observed_topology
        );
        assert!(matches!(
            source.with_mut(|runtime| runtime.reduce_pair(consumed)),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));
        assert_ne!(
            source.with_revisions(|_| ()).1.topology_revision(),
            observed_topology
        );
        assert!(target.retry_blocked_cursor(cursor));
        let (_, progress) = reduce_next_cursor(&mut target);
        match next_dependency {
            Some(next_dependency) => {
                assert_eq!(progress, CursorProgress::Blocked);
                assert!(matches!(
                    target.cursor_dependency(cursor),
                    Some(CursorDependency::SourceFrontier(observation))
                        if observation.endpoint() == DemandEndpoint::ActivePair(next_dependency)
                ));
            }
            None => {
                assert!(matches!(progress, CursorProgress::Materialized { .. }));
                assert!(target.cursor_dependency(cursor).is_none());
            }
        }
    }
}

#[test]
fn auxiliary_cursor_reinspects_after_a_principal_remote_cursor_materializes() {
    let mut leaf = NetBuilder::new();
    let value = leaf.data("leaf");
    let leaf = leaf.finish(value).instantiate_shared();

    let mut source: RuntimeNet<&'static str> = RuntimeNet::empty();
    let host = source.add_node(RuntimeNode::Bind);
    let source_cursor = source.begin_copy(leaf.prepare_copy_source());
    source.connect(Port::principal(host), Port::principal(source_cursor));
    let result = source.add_node(RuntimeNode::Data("result"));
    source.connect(Port::auxiliary(host, 2), Port::principal(result));
    let exposed = source.add_interface(Port::auxiliary(host, 1));
    source.exposed = Some(exposed);
    let cursor_pair = ActivePairKey::new(host, source_cursor);
    let source = SharedRuntimeNet::new(source);

    let mut target = target_waiting_on(source.clone());
    let (target_cursor, progress) = reduce_next_cursor(&mut target);
    assert_eq!(progress, CursorProgress::Blocked);
    assert!(matches!(
        target.cursor_dependency(target_cursor),
        Some(CursorDependency::SourceFrontier(observation))
            if observation.endpoint() == DemandEndpoint::ActivePair(cursor_pair)
    ));

    assert_eq!(
        source.with_mut(|runtime| claim_test_cursor(runtime, source_cursor)),
        Some(CursorProgress::Claimed)
    );
    assert!(matches!(
        source.advance_claimed_cursor(source_cursor),
        Some(CursorProgress::Materialized { .. })
    ));

    assert!(target.retry_blocked_cursor(target_cursor));
    assert_eq!(reduce_next_cursor(&mut target).1, CursorProgress::Blocked);
    let replacement_pair = match target
        .cursor_dependency(target_cursor)
        .expect("outer cursor should follow the host's new principal pair")
    {
        CursorDependency::SourceFrontier(observation) => {
            assert!(observation.source().ptr_eq(&source));
            let DemandEndpoint::ActivePair(pair) = observation.endpoint() else {
                panic!("replacement endpoint should remain an active pair");
            };
            pair
        }
        dependency => panic!("expected a replacement source pair, got {dependency:?}"),
    };
    assert_eq!(
        replacement_pair, cursor_pair,
        "the retained host node remains the active-pair key anchor"
    );
    source.with(|runtime| {
        let (left, right) = runtime
            .active_pair_nodes(replacement_pair)
            .expect("materialization should install a replacement principal partner");
        assert_ne!(left, source_cursor);
        assert_ne!(right, source_cursor);
        assert!(matches!(runtime.node(host), Some(RuntimeNode::Bind)));
        assert!(
            matches!(runtime.node(left), Some(RuntimeNode::Data("leaf")))
                || matches!(runtime.node(right), Some(RuntimeNode::Data("leaf")))
        );
    });
}

#[test]
fn materializing_a_root_creates_lazy_auxiliary_cursors() {
    let source = duplicated_argument_template().instantiate_shared();
    let source_nodes = source.with(|runtime| runtime.nodes.len());
    let mut target = RuntimeNet::empty();
    let local = target.add_node(RuntimeNode::Data(()));
    let cursor = target.begin_copy(source.prepare_copy_source());
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
fn logical_copy_preparation_does_not_reenter_the_target_net_lock() {
    let mut template = NetBuilder::<()>::new();
    let data = template.data(());
    let runtime = template.finish(data).instantiate_shared();
    let target = runtime.clone();
    let source = runtime.prepare_copy_source();
    let (sender, receiver) = mpsc::channel();

    let worker = thread::spawn(move || {
        let cursor = target.with_mut(|target| target.begin_copy(source));
        sender
            .send(cursor)
            .expect("copy-preparation test receiver should remain open");
    });

    let cursor = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("logical-copy preparation must not inspect the source under the target lock");
    worker
        .join()
        .expect("copy-preparation worker should not panic");
    remove_unwired_test_copy(&runtime, cursor);
}

#[test]
fn reciprocal_copy_installation_never_nests_runtime_net_locks() {
    let mut first_template = NetBuilder::<()>::new();
    let first_data = first_template.data(());
    let first = first_template.finish(first_data).instantiate_shared();
    let mut second_template = NetBuilder::<()>::new();
    let second_data = second_template.data(());
    let second = second_template.finish(second_data).instantiate_shared();

    let first_target = first.clone();
    let first_source = second.prepare_copy_source();
    let second_target = second.clone();
    let second_source = first.prepare_copy_source();
    let barrier = Arc::new(Barrier::new(3));
    let (sender, receiver) = mpsc::channel();

    let first_barrier = barrier.clone();
    let first_sender = sender.clone();
    let first_worker = thread::spawn(move || {
        let cursor = first_target.with_mut(|target| {
            first_barrier.wait();
            target.begin_copy(first_source)
        });
        first_sender
            .send((true, cursor))
            .expect("reciprocal-copy receiver should remain open");
    });
    let second_barrier = barrier.clone();
    let second_worker = thread::spawn(move || {
        let cursor = second_target.with_mut(|target| {
            second_barrier.wait();
            target.begin_copy(second_source)
        });
        sender
            .send((false, cursor))
            .expect("reciprocal-copy receiver should remain open");
    });

    barrier.wait();
    let installed = [
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("first reciprocal logical copy should complete"),
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("second reciprocal logical copy should complete"),
    ];
    first_worker
        .join()
        .expect("first reciprocal-copy worker should not panic");
    second_worker
        .join()
        .expect("second reciprocal-copy worker should not panic");

    for (installed_in_first, cursor) in installed {
        remove_unwired_test_copy(if installed_in_first { &first } else { &second }, cursor);
    }
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
    caller.resume_claimed_call_with_copy(call, source.prepare_copy_source());
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
    caller.resume_claimed_call_with_copy(call, source.prepare_copy_source());
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
    caller.resume_claimed_call_with_copy(call, source.prepare_copy_source());
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
        let frontier = claim.source.inspect_source_frontier(claim.remote);
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
        let cursor = target.begin_copy(source.prepare_copy_source());
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
    let cursor = target.begin_copy(source.prepare_copy_source());
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
