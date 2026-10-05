//! Generic port-and-wire interaction-net topology and reduction.
//!
//! Embedded data is supplied by the client. Immutable templates and runtime
//! nets allocate fan sites locally. Lazy copies translate source sites into
//! fresh target sites while preserving the complete residual history.

use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;

/// A packed port is `(node << 3) + index + 1`: two bits of port index, one
/// reserved sign bit, and the node above them. A plain [`Port`] keeps the
/// sign bit clear; a runtime [`Link`] stores its peer's sign there.
const PORT_BITS: u32 = 2;
const PORT_MASK: u64 = (1 << PORT_BITS) - 1;
pub(super) const SIGN_BIT: u64 = 1 << PORT_BITS;
const NODE_SHIFT: u32 = PORT_BITS + 1;

/// A port's polarity: it provides a value, or consumes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sign {
    Provides,
    Consumes,
}

impl Sign {
    pub fn opposite(self) -> Self {
        match self {
            Self::Provides => Self::Consumes,
            Self::Consumes => Self::Provides,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(NonZeroU64);

const _: () = assert!(std::mem::size_of::<NodeId>() == std::mem::size_of::<u64>());
const _: () = assert!(std::mem::size_of::<Option<NodeId>>() == std::mem::size_of::<u64>());

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

    #[cfg(all(test, feature = "interaction-net-profiling"))]
    pub(crate) fn for_test(site: u64) -> Self {
        Self::root(FanSite::from_raw(site))
    }
}

/// Client semantics embedded in otherwise generic interaction-net topology.
///
/// `Operator` values are immutable unary agents. Their principal port consumes
/// `Data`, and their sole auxiliary port is the result continuation. Both
/// callable-data interpretation and operator execution happen outside the
/// runtime-net mutex.
pub trait NetSpecialization: Clone + fmt::Debug + PartialEq + Eq + Sized + 'static {
    /// Semantic payloads are duplicated only through a runtime payload
    /// duplicator. They need no ambient `Clone`, formatting, or equality
    /// contract: managed specializations require matching access authority for
    /// all three observations.
    type Data: 'static;
    type Operator: 'static;
    /// Opaque identity retained when one runtime net refers to another.
    ///
    /// Generic topology clones, compares, and reports this identity, but does
    /// not assume how it owns or accesses the referenced net. The ordinary
    /// shared runtime uses `SharedRuntimeNet<Self>`; the core specialization
    /// may instead select a managed identity without changing `RuntimeNet`.
    type RuntimeSource: 'static;
    /// Opaque identity for one externally blocked operation.
    ///
    /// Equality lets an evaluator reject a stale wakeup after a pair has been
    /// retried and blocked on different work.
    type WaitToken: Clone + fmt::Debug + PartialEq + Eq + 'static;
    /// Structured explanation retained when specialization policy cannot
    /// reduce an otherwise valid active pair.
    type StuckReason: Clone + fmt::Debug + 'static;
    /// Linear evaluator state retained by a runtime-only callable checkpoint.
    ///
    /// Generic topology may move and visit this payload, but never clones,
    /// compares, or formats it. Only specialization policy may construct one.
    type CallableCheckpoint: Send + 'static;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorYield<S: NetSpecialization> {
    Data(S::Data),
    Operator(S::Operator),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Port(NonZeroU64);

const _: () = assert!(std::mem::size_of::<Port>() == std::mem::size_of::<u64>());
const _: () = assert!(std::mem::size_of::<Option<Port>>() == std::mem::size_of::<u64>());

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
        let max_node = (u64::MAX - SIGN_BIT - index - 1) >> NODE_SHIFT;
        assert!(
            node.get() <= max_node,
            "interaction-net packed port space exhausted"
        );
        let tagged = (node.get() << NODE_SHIFT) + index + 1;
        Self(NonZeroU64::new(tagged).expect("packed port is always nonzero"))
    }

    pub(super) fn raw(self) -> u64 {
        self.0.get()
    }

    pub(super) fn from_raw(raw: u64) -> Option<Self> {
        debug_assert_eq!(raw & SIGN_BIT, 0, "a plain port has no sign bit");
        NonZeroU64::new(raw).map(Self)
    }

    pub fn node(self) -> NodeId {
        NodeId::from_zero_based((self.0.get() - 1) >> NODE_SHIFT)
    }

    pub fn index(self) -> u32 {
        ((self.0.get() - 1) & PORT_MASK) as u32
    }

    pub fn is_principal(self) -> bool {
        self.index() == 0
    }
}

/// A port together with its own sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedPort {
    pub port: Port,
    pub sign: Sign,
}

impl SignedPort {
    pub fn new(port: Port, sign: Sign) -> Self {
        Self { port, sign }
    }
}

/// One stored runtime link: a reference to the peer port, typed by the
/// peer's sign. It packs into the same word as a plain port, with the peer's
/// sign in the reserved bit, so the polarity type costs no extra space.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Link(NonZeroU64);

impl Link {
    pub(super) fn to(peer: SignedPort) -> Self {
        let sign = match peer.sign {
            Sign::Provides => 0,
            Sign::Consumes => SIGN_BIT,
        };
        Self(NonZeroU64::new(peer.port.raw() | sign).expect("a packed port is nonzero"))
    }

    pub(super) fn peer(self) -> SignedPort {
        let raw = self.0.get();
        SignedPort {
            port: Port::from_raw(raw & !SIGN_BIT).expect("a link names a peer port"),
            sign: if raw & SIGN_BIT == 0 {
                Sign::Provides
            } else {
                Sign::Consumes
            },
        }
    }
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.peer().fmt(f)
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
    /// Function or application constructor. Ports: `[principal*, aux1,
    /// aux2]`, read as `[ap*, arg, result]` for an application and
    /// `[fn*, result, arg]` for a function. `aux1` consumes and `aux2`
    /// provides in both roles.
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

pub struct RuntimeCallableCheckpoint<Checkpoint> {
    pub(crate) generation: u64,
    pub(crate) payload: Option<Checkpoint>,
}

impl<Checkpoint> fmt::Debug for RuntimeCallableCheckpoint<Checkpoint> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeCallableCheckpoint")
            .field("generation", &self.generation)
            .field("payload", &self.payload.as_ref().map(|_| ".."))
            .finish()
    }
}

pub enum RuntimeNode<S: NetSpecialization> {
    Bind,
    Fan {
        identity: FanIdentity,
    },
    Erase,
    Data(S::Data),
    Operator(S::Operator),
    /// Opaque incremental WHNF state installed only by callable evaluation.
    CallableCheckpoint(RuntimeCallableCheckpoint<S::CallableCheckpoint>),
    /// Stable, evaluator-only anchor for a runtime net's exposed port.
    Interface,
    /// Evaluator-only one-way wire into a logical copy of another runtime net.
    RemoteCursor {
        copy: CopyId,
        remote: Port,
    },
}

impl<S: NetSpecialization> RuntimeNode<S> {
    /// The sign of auxiliary `index`, given the principal's sign, from the
    /// polarity table. A bind's auxiliaries are fixed; a fan's oppose its
    /// principal; an operator's result provides.
    pub(super) fn auxiliary_sign(&self, principal: Sign, index: u32) -> Sign {
        match (self, index) {
            (Self::Bind, 1) => Sign::Consumes,
            (Self::Bind, 2) => Sign::Provides,
            (Self::Fan { .. }, 1 | 2) => principal.opposite(),
            (Self::Operator(_), 1) => Sign::Provides,
            _ => unreachable!("node has no auxiliary {index}"),
        }
    }

    pub(super) fn port_count(&self) -> u32 {
        match self {
            Self::Bind | Self::Fan { .. } => 3,
            Self::Operator(_) | Self::Interface => 2,
            Self::Erase
            | Self::Data(_)
            | Self::CallableCheckpoint(_)
            | Self::RemoteCursor { .. } => 1,
        }
    }

    /// Clones only the ordinary topology/data vocabulary. Runtime evaluator
    /// checkpoints are deliberately linear and have no generic copy path.
    pub(super) fn duplicate_copyable(
        &self,
        duplicator: &impl super::runtime::RuntimeNetPayloadDuplicator<S>,
    ) -> Option<Self> {
        Some(match self {
            Self::Bind => Self::Bind,
            Self::Fan { identity } => Self::Fan {
                identity: identity.clone(),
            },
            Self::Erase => Self::Erase,
            Self::Data(data) => Self::Data(duplicator.duplicate_data(data)),
            Self::Operator(operator) => Self::Operator(duplicator.duplicate_operator(operator)),
            Self::Interface => Self::Interface,
            Self::RemoteCursor { copy, remote } => Self::RemoteCursor {
                copy: *copy,
                remote: *remote,
            },
            Self::CallableCheckpoint(_) => return None,
        })
    }
}

impl<S: NetSpecialization> fmt::Debug for RuntimeNode<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind => formatter.write_str("Bind"),
            Self::Fan { identity } => formatter
                .debug_struct("Fan")
                .field("identity", identity)
                .finish(),
            Self::Erase => formatter.write_str("Erase"),
            Self::Data(_) => formatter.write_str("Data(..)"),
            Self::Operator(_) => formatter.write_str("Operator(..)"),
            Self::CallableCheckpoint(_) => formatter.write_str("CallableCheckpoint(..)"),
            Self::Interface => formatter.write_str("Interface"),
            Self::RemoteCursor { copy, remote } => formatter
                .debug_struct("RemoteCursor")
                .field("copy", copy)
                .field("remote", remote)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CopyId(pub(super) u64);

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

#[derive(Clone)]
pub struct InteractionNet<S: NetSpecialization> {
    pub(super) nodes: Arc<[Node<S>]>, // nodes identified by index
    /// Every wire between ports, stored provider-first: `left` provides and
    /// `right` consumes.
    pub(super) wires: Arc<[Wire]>,
    pub(super) exposed: Port, // closed net has one exposed port; it provides
    /// False for a test template built deliberately unpolarized; its wire
    /// orientation is then arbitrary.
    #[cfg(test)]
    pub(super) polarized: bool,
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
}
