//! Closed I4A managed-value shell and leaf-policy fixtures.
//!
//! This module selects and verifies the initial integration shape without
//! publishing a production `core::Value` containing a bare managed edge. The
//! first production shell remains gated on I4B-I4F. Recursive variants use one
//! representative semantic edge here; later checkpoints replace that fixture
//! edge with each concrete payload family's exact visitor.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use glam_gc::{Gc, Trace, Visitor};

use super::{ManagedDropRecord, ManagedFamily, managed_slot_extent};
use crate::core::{Atom, Builtin, CoreValueAllocationScope, CoreValueFactory, Key, Value};
use crate::number::Number;
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagedValueKind {
    Atom,
    Number,
    Binary,
    List,
    Dict,
    Builtin,
    PartialBuiltin,
    Function,
    Net,
    Lazy,
    Promised,
    Metadata,
    Opaque,
}

impl ManagedValueKind {
    const ALL: [Self; 13] = [
        Self::Atom,
        Self::Number,
        Self::Binary,
        Self::List,
        Self::Dict,
        Self::Builtin,
        Self::PartialBuiltin,
        Self::Function,
        Self::Net,
        Self::Lazy,
        Self::Promised,
        Self::Metadata,
        Self::Opaque,
    ];

    /// Compile-exhaustive correspondence with the compatibility value shell.
    ///
    /// This does not inspect or convert a production value. Adding a current
    /// `Value` variant forces I4's managed dispatch inventory to change in the
    /// same build.
    fn of_compatibility_value(value: &Value) -> Self {
        match value {
            Value::Atom(_) => Self::Atom,
            Value::Number(_) => Self::Number,
            Value::Binary(_) => Self::Binary,
            Value::List(_) => Self::List,
            Value::Dict(_) => Self::Dict,
            Value::Builtin(_) => Self::Builtin,
            Value::PartialBuiltin(_) => Self::PartialBuiltin,
            Value::Function(_) => Self::Function,
            Value::Net(_) => Self::Net,
            Value::Lazy(_) => Self::Lazy,
            Value::Promised(_) => Self::Promised,
            Value::Metadata(_) => Self::Metadata,
            Value::Opaque(_) => Self::Opaque,
        }
    }
}

struct ShellEdge(Mutex<Option<Gc<ManagedValueNode>>>);

impl ShellEdge {
    fn empty() -> Self {
        Self(Mutex::new(None))
    }

    fn to(target: Gc<ManagedValueNode>) -> Self {
        Self(Mutex::new(Some(target)))
    }

    fn get(&self) -> Option<Gc<ManagedValueNode>> {
        *self.0.lock().expect("managed shell edge must not poison")
    }
}

/// Representative initial shell granularity.
///
/// Approved leaf payloads are stored directly. Every recursive family reports
/// a semantic managed edge without committing the visitor to a Rust field
/// offset. `OpaqueBoundary` deliberately carries no payload: I4B/I10B.0 own
/// the real opaque representation decision.
enum ManagedValueShell {
    Atom(Atom),
    Number(Number),
    Binary(Bytes),
    List(ShellEdge),
    Dict(ShellEdge),
    Builtin(Builtin),
    PartialBuiltin(ShellEdge),
    Function(ShellEdge),
    Net(ShellEdge),
    Lazy(ShellEdge),
    Promised(ShellEdge),
    Metadata(ShellEdge),
    OpaqueBoundary,
}

impl ManagedValueShell {
    fn kind(&self) -> ManagedValueKind {
        match self {
            Self::Atom(_) => ManagedValueKind::Atom,
            Self::Number(_) => ManagedValueKind::Number,
            Self::Binary(_) => ManagedValueKind::Binary,
            Self::List(_) => ManagedValueKind::List,
            Self::Dict(_) => ManagedValueKind::Dict,
            Self::Builtin(_) => ManagedValueKind::Builtin,
            Self::PartialBuiltin(_) => ManagedValueKind::PartialBuiltin,
            Self::Function(_) => ManagedValueKind::Function,
            Self::Net(_) => ManagedValueKind::Net,
            Self::Lazy(_) => ManagedValueKind::Lazy,
            Self::Promised(_) => ManagedValueKind::Promised,
            Self::Metadata(_) => ManagedValueKind::Metadata,
            Self::OpaqueBoundary => ManagedValueKind::Opaque,
        }
    }

    /// Reports semantic edges without exposing field offsets to the caller.
    fn visit_edges(&self, mut visit: impl FnMut(Gc<ManagedValueNode>)) {
        let edge = match self {
            Self::Atom(atom) => {
                let _: &Atom = atom;
                None
            }
            Self::Number(number) => {
                let _: &Number = number;
                None
            }
            Self::Binary(bytes) => {
                let _: &Bytes = bytes;
                None
            }
            Self::Builtin(builtin) => {
                let _: &Builtin = builtin;
                None
            }
            Self::OpaqueBoundary => None,
            Self::List(edge)
            | Self::Dict(edge)
            | Self::PartialBuiltin(edge)
            | Self::Function(edge)
            | Self::Net(edge)
            | Self::Lazy(edge)
            | Self::Promised(edge)
            | Self::Metadata(edge) => edge.get(),
        };
        if let Some(edge) = edge {
            visit(edge);
        }
    }
}

struct ManagedValueNode {
    shell: ManagedValueShell,
    drops: Arc<AtomicUsize>,
}

impl ManagedValueNode {
    fn new(shell: ManagedValueShell, drops: &Arc<AtomicUsize>) -> Self {
        Self {
            shell,
            drops: Arc::clone(drops),
        }
    }
}

impl Drop for ManagedValueNode {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: `ManagedValueShell::visit_edges` exhaustively classifies the current
// shell and reports the one representative managed edge held by every
// recursive fixture variant. Leaf variants and the unpopulated opaque boundary
// contain no managed edge. The callback is synchronous and never retained.
unsafe impl Trace for ManagedValueNode {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        self.shell.visit_edges(|edge| visitor.visit(edge));
    }
}

// SAFETY: direct destruction only updates an external atomic test observer.
// Transitive destruction releases passive Atom/Number/Bytes, mutex, Gc, and
// Arc representations. It neither invokes a Glam service nor observes or
// preserves a dying shell edge. The opaque fixture carries no payload.
unsafe impl ManagedFamily for ManagedValueNode {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "I4A closed managed value shell",
        "src/core/managed/value_shell.rs",
        "direct Drop updates only an external atomic counter",
        "shell leaves and synchronization resources drop passively; Gc edges are inert",
    );
}

fn values() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

fn leaf_shells() -> [ManagedValueShell; 4] {
    [
        ManagedValueShell::Atom(Atom::from_key(&Key::binary_from_text("leaf"))),
        ManagedValueShell::Number(Number::from_ratio_i64(3, 5).expect("nonzero denominator")),
        ManagedValueShell::Binary(Bytes::from_static(b"leaf")),
        ManagedValueShell::Builtin(Builtin::Append),
    ]
}

fn inspect_node<'scope>(
    scope: &'scope CoreValueAllocationScope<'scope>,
    node: Gc<ManagedValueNode>,
) -> &'scope ManagedValueNode {
    // SAFETY: every caller keeps the freshly allocated node live throughout
    // this exact mutator scope and uses the matching managed family and heap.
    unsafe { scope.get_traced_edge(node) }
}

#[test]
fn managed_leaf_families_trace_zero_edges() {
    #[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
    assert_eq!(
        (
            std::mem::size_of::<ManagedValueNode>(),
            std::mem::align_of::<ManagedValueNode>(),
        ),
        (72, 8),
        "the reviewed x86-64 I4A fixture layout changed",
    );
    let values = values();
    let baseline = values
        .collect_managed_for_test()
        .expect("canonical roots should collect before the shell fixture");
    let drops = Arc::new(AtomicUsize::new(0));
    let roots = values.with_managed_values(|scope| {
        let allocator = scope
            .allocator::<ManagedValueNode>()
            .expect("the selected managed shell layout should be supported");
        assert_eq!(
            <ManagedValueNode as Trace>::REQUESTED_SLOT_SIZE,
            Some(managed_slot_extent::<ManagedValueNode>())
        );

        leaf_shells()
            .into_iter()
            .map(|shell| {
                let node = allocator.alloc(ManagedValueNode::new(shell, &drops));
                let mut edges = 0;
                inspect_node(&scope, node).shell.visit_edges(|_| edges += 1);
                assert_eq!(edges, 0);
                scope.root(node)
            })
            .collect::<Vec<_>>()
    });

    let live = values
        .collect_managed_for_test()
        .expect("rooted managed leaf shells should collect");
    assert_eq!(live.marked_slots(), baseline.marked_slots() + roots.len());
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    drop(roots);
    let dead = values
        .collect_managed_for_test()
        .expect("unrooted managed leaf shells should collect");
    assert_eq!(dead.finalized_slots(), 4);
    assert_eq!(drops.load(Ordering::Relaxed), 4);
}

#[test]
fn managed_value_shell_dispatches_every_variant() {
    let values = values();
    let drops = Arc::new(AtomicUsize::new(0));

    values.with_managed_values(|scope| {
        let allocator = scope
            .allocator::<ManagedValueNode>()
            .expect("the selected managed shell layout should be supported");
        let child = allocator.alloc(ManagedValueNode::new(
            ManagedValueShell::Builtin(Builtin::Append),
            &drops,
        ));
        let mut shells = leaf_shells().into_iter().collect::<Vec<_>>();
        shells.extend([
            ManagedValueShell::List(ShellEdge::to(child)),
            ManagedValueShell::Dict(ShellEdge::to(child)),
            ManagedValueShell::PartialBuiltin(ShellEdge::to(child)),
            ManagedValueShell::Function(ShellEdge::to(child)),
            ManagedValueShell::Net(ShellEdge::to(child)),
            ManagedValueShell::Lazy(ShellEdge::to(child)),
            ManagedValueShell::Promised(ShellEdge::to(child)),
            ManagedValueShell::Metadata(ShellEdge::to(child)),
            ManagedValueShell::OpaqueBoundary,
        ]);

        let mut observed = Vec::with_capacity(shells.len());
        for shell in shells {
            let expected_edges = usize::from(!matches!(
                shell,
                ManagedValueShell::Atom(_)
                    | ManagedValueShell::Number(_)
                    | ManagedValueShell::Binary(_)
                    | ManagedValueShell::Builtin(_)
                    | ManagedValueShell::OpaqueBoundary
            ));
            let node = allocator.alloc(ManagedValueNode::new(shell, &drops));
            let node = inspect_node(&scope, node);
            let mut edges = Vec::new();
            node.shell.visit_edges(|edge| edges.push(edge));
            assert_eq!(edges.len(), expected_edges);
            assert!(edges.into_iter().all(|edge| edge.ptr_eq(child)));
            observed.push(node.shell.kind());
        }

        observed.sort_by_key(|kind| *kind as usize);
        let mut expected = ManagedValueKind::ALL;
        expected.sort_by_key(|kind| *kind as usize);
        assert_eq!(observed, expected);
        let _: fn(&Value) -> ManagedValueKind = ManagedValueKind::of_compatibility_value;
    });
}

#[test]
fn managed_value_shell_cycle_marks_once() {
    let values = values();
    let baseline = values
        .collect_managed_for_test()
        .expect("canonical roots should collect before the shell-cycle fixture");
    let drops = Arc::new(AtomicUsize::new(0));
    let root = values.with_managed_values(|scope| {
        let allocator = scope
            .allocator::<ManagedValueNode>()
            .expect("the selected managed shell layout should be supported");
        let node = allocator.alloc(ManagedValueNode::new(
            ManagedValueShell::List(ShellEdge::empty()),
            &drops,
        ));
        let edge = match &inspect_node(&scope, node).shell {
            ManagedValueShell::List(edge) => edge,
            _ => unreachable!("the fixture was constructed as a list shell"),
        };

        // SAFETY: `node` is a live owner and target in this matching heap. The
        // mutex contains no edge before the closure and exactly `node` after.
        unsafe {
            scope
                .mutator
                .with_edge_replacement(node, None, Some(node), || {
                    *edge.0.lock().expect("managed shell edge must not poison") = Some(node);
                });
        }
        scope.root(node)
    });

    let live = values
        .collect_managed_for_test()
        .expect("the rooted shell cycle should collect");
    assert_eq!(live.root_entries(), baseline.root_entries() + 1);
    assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    drop(root);
    let dead = values
        .collect_managed_for_test()
        .expect("the unrooted shell cycle should be reclaimed");
    assert_eq!(dead.root_entries(), baseline.root_entries());
    assert_eq!(dead.finalized_slots(), 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
