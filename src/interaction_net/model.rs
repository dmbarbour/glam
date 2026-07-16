//! Generic port-and-wire interaction-net topology and reduction.
//!
//! Embedded data is supplied by the client. Immutable templates and runtime
//! nets allocate fan sites locally. Lazy copies translate source sites into
//! fresh target sites while preserving the complete residual history.

use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;

use super::runtime::SharedRuntimeNet;

const PORT_BITS: u32 = 2;
const PORT_MASK: u64 = (1 << PORT_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(NonZeroU64);

impl NodeId {
    pub(super) fn from_index(index: usize) -> Self {
        Self::from_zero_based(
            u64::try_from(index).expect("interaction-net node index does not fit in u64"),
        )
    }

    pub(super) fn from_zero_based(index: u64) -> Self {
        let encoded = index
            .checked_add(1)
            .expect("interaction-net node ID space exhausted");
        Self(NonZeroU64::new(encoded).expect("encoded node ID is always nonzero"))
    }

    pub(super) fn index(self) -> usize {
        usize::try_from(self.get()).expect("interaction-net node ID does not fit in usize")
    }

    pub fn get(self) -> u64 {
        self.0.get() - 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FanSite(pub(super) u64);

impl FanSite {
    pub fn get(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(super) const fn from_raw(site: u64) -> Self {
        Self(site)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DuplicationStep {
    pub through: FanIdentity,
    pub branch: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FanIdentity {
    pub site: FanSite,
    pub context: Arc<[DuplicationStep]>,
}

impl FanIdentity {
    pub(super) fn root(site: FanSite) -> Self {
        Self {
            site,
            context: Arc::from([]),
        }
    }

    pub(super) fn residual(&self, through: &Self, branch: u8) -> Self {
        let mut context = self.context.to_vec();
        context.push(DuplicationStep {
            through: through.clone(),
            branch,
        });
        Self {
            site: self.site,
            context: Arc::from(context),
        }
    }
}

/// Client semantics embedded in otherwise generic interaction-net topology.
///
/// `Operator` values are immutable unary agents. Their principal port consumes
/// `Data`, and their sole auxiliary port is the result continuation. Both
/// callable-data interpretation and operator execution happen outside the
/// runtime-net mutex.
pub trait NetSpecialization: Clone + fmt::Debug + PartialEq + Eq + Sized + 'static {
    type Data: Clone + fmt::Debug + PartialEq + Eq + 'static;
    type Operator: Clone + fmt::Debug + PartialEq + Eq + 'static;
    type Error: fmt::Display;

    fn callable(data: Self::Data) -> Result<Callable<Self>, Self::Error>;

    fn apply_operator(
        operator: &Self::Operator,
        data: &Self::Data,
    ) -> Result<OperatorYield<Self>, Self::Error>;

    fn operator_name(operator: &Self::Operator) -> &str;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorYield<S: NetSpecialization> {
    Data(S::Data),
    Operator(S::Operator),
}

/// The topology that callable data exposes when it meets a [`Node::Bind`].
///
/// A shared net is loaded through a lazy logical copy. An operator is
/// installed behind a fresh bind so the ordinary bind-join rule performs the
/// application.
pub enum Callable<S: NetSpecialization> {
    Net(SharedRuntimeNet<S>),
    Operator(S::Operator),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Port(NonZeroU64);

impl Port {
    pub fn principal(node: NodeId) -> Self {
        Self::new(node, 0)
    }

    pub fn auxiliary(node: NodeId, index: u32) -> Self {
        assert!(
            (1..=2).contains(&index),
            "auxiliary port index must be 1 or 2"
        );
        Self::new(node, index)
    }

    pub(super) fn new(node: NodeId, index: u32) -> Self {
        let index = u64::from(index);
        let max_node = (u64::MAX - index - 1) >> PORT_BITS;
        assert!(
            node.get() <= max_node,
            "interaction-net packed port space exhausted"
        );
        let tagged = (node.get() << PORT_BITS) + index + 1;
        Self(NonZeroU64::new(tagged).expect("packed port is always nonzero"))
    }

    pub fn node(self) -> NodeId {
        NodeId::from_zero_based((self.0.get() - 1) >> PORT_BITS)
    }

    pub fn index(self) -> u32 {
        ((self.0.get() - 1) & PORT_MASK) as u32
    }

    pub fn is_principal(self) -> bool {
        self.index() == 0
    }
}

impl fmt::Debug for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Port")
            .field("node", &self.node())
            .field("index", &self.index())
            .finish()
    }
}

/// Immutable nodes in a reusable interaction-net template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node<S: NetSpecialization> {
    /// Function or application constructor. Ports: `[ap*, arg, result]`.
    Bind,
    /// Binary Lamping-style fan. Ports: `[input*, left, right]`.
    Fan { site: FanSite },
    /// Eraser for a value used zero times. Port: `[input*]`.
    Erase,
    /// Client-defined embedded data. Port: `[data*]`.
    Data(S::Data),
    /// Client-defined unary data transition. Ports: `[input*, result]`.
    Operator(S::Operator),
}

impl<S: NetSpecialization> Node<S> {
    pub(super) fn port_count(&self) -> u32 {
        match self {
            Self::Bind | Self::Fan { .. } => 3,
            Self::Operator(_) => 2,
            Self::Erase | Self::Data(_) => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeNode<S: NetSpecialization> {
    Bind,
    Fan {
        identity: FanIdentity,
    },
    Erase,
    Data(S::Data),
    Operator(S::Operator),
    /// Stable, evaluator-only anchor for a runtime net's exposed port.
    Interface,
    /// Evaluator-only one-way wire into a logical copy of another runtime net.
    RemoteCursor {
        copy: CopyId,
        remote: Port,
    },
}

impl<S: NetSpecialization> RuntimeNode<S> {
    pub(super) fn port_count(&self) -> u32 {
        match self {
            Self::Bind | Self::Fan { .. } => 3,
            Self::Operator(_) | Self::Interface => 2,
            Self::Erase | Self::Data(_) | Self::RemoteCursor { .. } => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CopyId(pub(super) u64);

impl CopyId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wire {
    pub left: Port,  // port including node ID and index
    pub right: Port, // each port is wired to exactly one other port (except the exposed port)
}

/// Stable key for a principal-principal wire.
///
/// A principal port has at most one neighbor, so the lower-numbered 
/// endpoint uniquely identifies the pair. The other endpoint is always 
/// recovered from the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActivePairKey(NodeId);

impl ActivePairKey {
    pub(super) fn new(left: NodeId, right: NodeId) -> Self {
        Self(left.min(right))
    }

    pub fn node(self) -> NodeId {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionNet<S: NetSpecialization> {
    pub(super) nodes: Arc<[Node<S>]>, // nodes identified by index
    pub(super) wires: Arc<[Wire]>,    // all wires between ports
    pub(super) exposed: Port,         // closed net has one exposed port
    pub(super) active_pairs: Arc<[ActivePairKey]>, // principal-principal wires
}

#[cfg(test)]
impl<S: NetSpecialization> InteractionNet<S> {
    pub fn nodes(&self) -> &[Node<S>] {
        &self.nodes
    }

    pub fn wires(&self) -> &[Wire] {
        &self.wires
    }

    pub fn exposed(&self) -> Port {
        self.exposed
    }

    pub fn active_pairs(&self) -> &[ActivePairKey] {
        &self.active_pairs
    }
}
