//! Core operators and specialization for generic interaction nets.
//!
//! Front-end semantic lowering lives in `g_syntax`; this module deliberately
//! contains no expression language.

use std::sync::Arc;

use crate::core::{
    BuiltinCall, CoreValueFactory, FunctionCode, Key, RuntimeValueAccess, RuntimeValueObserver,
    Value,
};
use crate::evaluation::EvaluationWaitToken;
use crate::interaction_net::{
    ActivePairKey, ActivePairStep, BlockedCall, BlockedOperatorCall, CursorDependency,
    CursorDependencyDisposition, CursorDependencyResolution, CursorProgress, CursorStep,
    DemandEndpoint, FrontierObservation, InteractionNet, InterfaceDemand, NetContention, NodeId,
    NormalizationBatchLease, Port, PreparedCopySource, Reduction, RuntimeNet, RuntimeNetRevisions,
    SharedRuntimeNet,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreDataKey {
    Key(Key),
    Index,
    PathIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreOperator {
    ApplyArity {
        arity: usize,
        supplied: Arc<[Value]>,
    },
    FunctionCaptures {
        code: Arc<FunctionCode>,
        supplied: Arc<[Value]>,
    },
    ComputationCaptures {
        code: Arc<FunctionCode>,
        supplied: Arc<[Value]>,
    },
    Dict {
        keys: Arc<[Key]>,
        supplied: Arc<[Value]>,
    },
    Builtin(BuiltinCall),
    Applicable(Value),
    List {
        arity: usize,
        supplied: Arc<[Value]>,
    },
    Access {
        path: Arc<[CoreDataKey]>,
        supplied: Arc<[Value]>,
    },
    /// Reifies an opaque-tagged external effect request without performing it
    /// during interaction-net evaluation.
    Request {
        tag: Key,
        arity: usize,
        supplied: Arc<[Value]>,
        wrap_effect: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreSpecialization;

/// Opaque identity for evaluator work that suspends a core net call. The weak
/// session provenance remains hidden from the generic runtime, which only
/// clones and compares tokens for exact wakeups.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoreWaitToken(pub(crate) EvaluationWaitToken);

impl CoreWaitToken {
    pub(crate) fn wait_id(&self) -> u64 {
        self.0.get()
    }
}

pub type CoreInteractionNet = InteractionNet<CoreSpecialization>;

/// Runtime-local identity of one shared core interaction net.
///
/// The generic shared owner remains private. The weak value-domain observer
/// records which runtime may inspect the semantic values in this net without
/// retaining that runtime after its explicit owners disappear. I3D.3b moves
/// every locking operation below a matching scoped `RuntimeValueAccess`; this
/// checkpoint first closes construction and returned-observation escape paths.
#[derive(Clone)]
pub(crate) struct CoreRuntimeNet {
    inner: SharedRuntimeNet<CoreSpecialization>,
    values: RuntimeValueObserver,
}

/// One bounded, thread-local authority to inspect or mutate a core net.
///
/// The view borrows both the durable net and the matching managed-value access
/// carrier. It cannot enter a work descriptor, survive the mutator region, or
/// cross a thread. The generic shared owner remains hidden behind this view.
pub(crate) struct CoreRuntimeNetAccess<'access, 'scope> {
    runtime: &'access CoreRuntimeNet,
    _values: &'access RuntimeValueAccess<'scope>,
}

impl CoreValueFactory {
    /// Instantiates a core net in this factory's exact value domain.
    pub(crate) fn instantiate_core_net(&self, template: &CoreInteractionNet) -> CoreRuntimeNet {
        CoreRuntimeNet {
            inner: template.instantiate_shared(),
            values: self.runtime_value_observer(),
        }
    }

    #[cfg(test)]
    pub(crate) fn adopt_core_net_for_test(
        &self,
        inner: SharedRuntimeNet<CoreSpecialization>,
    ) -> CoreRuntimeNet {
        CoreRuntimeNet {
            inner,
            values: self.runtime_value_observer(),
        }
    }
}

impl CoreRuntimeNet {
    /// Instantiates topology whose payloads were assembled from values already
    /// admitted by this net's domain.
    pub(crate) fn instantiate_related(&self, template: &CoreInteractionNet) -> Self {
        Self {
            inner: template.instantiate_shared(),
            values: self.values.clone(),
        }
    }

    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        let same_net = self.inner.ptr_eq(&other.inner);
        debug_assert!(
            !same_net || self.values.same_domain(&other.values),
            "one core runtime net cannot carry multiple value domains"
        );
        same_net
    }

    /// Derives bounded net access from matching value-domain authority.
    pub(crate) fn access<'access, 'scope>(
        &'access self,
        values: &'access RuntimeValueAccess<'scope>,
    ) -> CoreRuntimeNetAccess<'access, 'scope> {
        assert!(
            values.admits(&self.values),
            "core net belongs to evaluation runtime {}, but access came from evaluation runtime {}",
            self.values.runtime_id().get(),
            values.runtime_id().get()
        );
        CoreRuntimeNetAccess {
            runtime: self,
            _values: values,
        }
    }

    /// Transitional I3D.3c exception: the returned lease may lock on close or
    /// drop and therefore cannot yet borrow the access scope.
    pub(crate) fn try_begin_normalization_batch(
        &self,
    ) -> Result<NormalizationBatchLease<CoreSpecialization>, CoreNetContention> {
        self.inner
            .try_begin_normalization_batch()
            .map_err(|contention| CoreNetContention::new(self, contention))
    }

    #[cfg(test)]
    pub(crate) fn belongs_to(&self, values: &CoreValueFactory) -> bool {
        self.values.belongs_to(values)
    }

    #[cfg(test)]
    pub(crate) fn domain_is_live(&self) -> bool {
        self.values.is_live()
    }

    #[cfg(test)]
    pub(crate) fn with_test_access<R>(
        &self,
        operation: impl FnOnce(CoreRuntimeNetAccess<'_, '_>) -> R,
    ) -> R {
        let values = self
            .values
            .upgrade()
            .expect("a test cannot inspect a core net after its value domain is dropped");
        values.with_runtime_value_access(|access| operation(self.access(&access)))
    }

    #[cfg(test)]
    pub(crate) fn test_with<R>(
        &self,
        inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R,
    ) -> R {
        self.with_test_access(|access| access.with(inspect))
    }

    #[cfg(test)]
    pub(crate) fn test_with_revisions<R>(
        &self,
        inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R,
    ) -> (R, RuntimeNetRevisions) {
        self.with_test_access(|access| access.with_revisions(inspect))
    }

    #[cfg(test)]
    pub(crate) fn test_with_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> R,
    ) -> R {
        self.with_test_access(|access| access.with_mut(update))
    }

    #[cfg(test)]
    pub(crate) fn test_with_optional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> Option<R>,
    ) -> Option<R> {
        self.with_test_access(|access| access.with_optional_mut(update))
    }

    #[cfg(test)]
    pub(crate) fn test_poll_interface_demand(&self, interface: Port) -> InterfaceDemand {
        self.with_test_access(|access| access.poll_interface_demand(interface))
    }

    #[cfg(test)]
    pub(crate) fn test_step_cursor(&self, cursor: NodeId) -> CoreCursorStep {
        self.with_test_access(|access| access.step_cursor(cursor))
    }

    #[cfg(test)]
    pub(crate) fn test_advance_claimed_cursor(&self, cursor: NodeId) -> Option<CursorProgress> {
        self.with_test_access(|access| access.advance_claimed_cursor(cursor))
    }

    #[cfg(test)]
    pub(crate) fn test_prepare_copy_source(&self) -> CorePreparedCopySource {
        self.with_test_access(|access| access.prepare_copy_source())
    }

    #[cfg(test)]
    pub(crate) fn active_normalization_batch(&self) -> Option<(u64, bool)> {
        self.inner.active_normalization_batch()
    }

    #[cfg(test)]
    pub(crate) fn test_stable_auxiliary(values: &CoreValueFactory) -> (Self, Port) {
        let (inner, interface) = SharedRuntimeNet::test_stable_auxiliary();
        (values.adopt_core_net_for_test(inner), interface)
    }

    #[cfg(test)]
    pub(crate) fn test_copy_layer(source: Self) -> (Self, Port) {
        let values = source.values.clone();
        let (inner, interface) = SharedRuntimeNet::test_copy_layer(source.inner);
        (Self { inner, values }, interface)
    }

    #[cfg(test)]
    pub(crate) fn test_pair_owned_copy_layer(source: Self) -> (Self, Port, NodeId) {
        let values = source.values.clone();
        let (inner, interface, cursor) = SharedRuntimeNet::test_pair_owned_copy_layer(source.inner);
        (Self { inner, values }, interface, cursor)
    }

    #[cfg(test)]
    pub(crate) fn test_productive_pair_owned_copy_layer(source: Self) -> (Self, Port) {
        let values = source.values.clone();
        let (inner, interface) =
            SharedRuntimeNet::test_productive_pair_owned_copy_layer(source.inner);
        (Self { inner, values }, interface)
    }

    #[cfg(test)]
    pub(crate) fn test_stable_root_with_claimed_cursor(source: Self) -> (Self, Port, NodeId) {
        let values = source.values.clone();
        let (inner, interface, cursor) =
            SharedRuntimeNet::test_stable_root_with_claimed_cursor(source.inner);
        (Self { inner, values }, interface, cursor)
    }

    #[cfg(test)]
    pub(crate) fn test_claim_pairless_cursor_obligation(&self, cursor: NodeId) -> bool {
        self.inner.test_claim_pairless_cursor_obligation(cursor)
    }
}

impl CoreRuntimeNetAccess<'_, '_> {
    pub(crate) fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R) -> R {
        self.runtime.inner.with(inspect)
    }

    #[cfg(test)]
    pub(crate) fn with_revisions<R>(
        &self,
        inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R,
    ) -> (R, RuntimeNetRevisions) {
        self.runtime.inner.with_revisions(inspect)
    }

    pub(crate) fn with_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> R,
    ) -> R {
        self.runtime.inner.with_mut(update)
    }

    #[cfg(test)]
    pub(crate) fn with_optional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> Option<R>,
    ) -> Option<R> {
        self.runtime.inner.with_optional_mut(update)
    }

    pub(crate) fn poll_interface_demand(&self, interface: Port) -> InterfaceDemand {
        self.runtime.inner.poll_interface_demand(interface)
    }

    pub(crate) fn resolve_cursor_dependency(
        &self,
        cursor: NodeId,
        expected: &CoreCursorDependency,
        disposition: CursorDependencyDisposition,
    ) -> CursorDependencyResolution {
        self.runtime
            .inner
            .resolve_cursor_dependency(cursor, &expected.to_generic(), disposition)
    }

    pub(crate) fn step_cursor(&self, cursor: NodeId) -> CoreCursorStep {
        CoreCursorStep::from_generic(self.runtime, self.runtime.inner.step_cursor(cursor))
    }

    pub(crate) fn step_active_pair(&self, pair: ActivePairKey) -> CoreActivePairStep {
        CoreActivePairStep::from_generic(self.runtime, self.runtime.inner.step_active_pair(pair))
    }

    pub(crate) fn advance_claimed_cursor(&self, cursor: NodeId) -> Option<CursorProgress> {
        self.runtime.inner.advance_claimed_cursor(cursor)
    }

    pub(crate) fn prepare_copy_source(&self) -> CorePreparedCopySource {
        CorePreparedCopySource {
            inner: self.runtime.inner.prepare_copy_source(),
            values: self.runtime.values.clone(),
        }
    }

    pub(crate) fn resume_claimed_call_with_copy(
        &self,
        call: crate::interaction_net::Call,
        source: CorePreparedCopySource,
    ) {
        let source = source.into_inner_for(&self.runtime.values);
        self.runtime
            .inner
            .with_mut(|runtime| runtime.resume_claimed_call_with_copy(call, source));
    }
}

impl std::fmt::Debug for CoreRuntimeNet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("CoreRuntimeNet")
            .field(&self.inner)
            .finish()
    }
}

impl PartialEq for CoreRuntimeNet {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

impl Eq for CoreRuntimeNet {}

pub(crate) struct CorePreparedCopySource {
    inner: PreparedCopySource<CoreSpecialization>,
    values: RuntimeValueObserver,
}

impl CorePreparedCopySource {
    fn into_inner_for(
        self,
        target: &RuntimeValueObserver,
    ) -> PreparedCopySource<CoreSpecialization> {
        assert!(
            target.same_domain(&self.values),
            "a core net cannot copy topology from another value domain"
        );
        self.inner
    }
}

#[derive(Clone)]
pub(crate) struct CoreNetContention {
    runtime: CoreRuntimeNet,
    revisions: RuntimeNetRevisions,
}

impl CoreNetContention {
    fn new(runtime: &CoreRuntimeNet, contention: NetContention<CoreSpecialization>) -> Self {
        debug_assert!(contention.runtime().ptr_eq(&runtime.inner));
        Self {
            runtime: runtime.clone(),
            revisions: contention.revisions(),
        }
    }

    pub(crate) fn wait_for_disturbance(&self) {
        self.runtime
            .inner
            .wait_for_disturbance(self.revisions.disturbance_epoch());
    }
}

impl std::fmt::Debug for CoreNetContention {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoreNetContention")
            .field("runtime", &self.runtime)
            .field("revisions", &self.revisions)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CoreFrontierObservation {
    inner: FrontierObservation<CoreSpecialization>,
    source: CoreRuntimeNet,
}

impl CoreFrontierObservation {
    fn from_generic(
        owner: &CoreRuntimeNet,
        inner: FrontierObservation<CoreSpecialization>,
    ) -> Self {
        let source = CoreRuntimeNet {
            inner: inner.source().clone(),
            values: owner.values.clone(),
        };
        Self { inner, source }
    }

    pub(crate) fn source(&self) -> &CoreRuntimeNet {
        &self.source
    }

    pub(crate) fn endpoint(&self) -> DemandEndpoint {
        self.inner.endpoint()
    }

    pub(crate) fn step_active_pair(
        &self,
        access: &CoreRuntimeNetAccess<'_, '_>,
        pair: ActivePairKey,
    ) -> CoreActivePairStep {
        assert!(
            self.source.ptr_eq(access.runtime),
            "frontier observation requires access to its source net"
        );
        CoreActivePairStep::from_generic(&self.source, self.inner.step_active_pair(pair))
    }

    pub(crate) fn step_cursor(
        &self,
        access: &CoreRuntimeNetAccess<'_, '_>,
        cursor: NodeId,
    ) -> CoreCursorStep {
        assert!(
            self.source.ptr_eq(access.runtime),
            "frontier observation requires access to its source net"
        );
        CoreCursorStep::from_generic(&self.source, self.inner.step_cursor(cursor))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CoreCursorDependency {
    LocalCursor(NodeId),
    SourceCursor(CoreFrontierObservation),
    SourceFrontier(CoreFrontierObservation),
}

impl CoreCursorDependency {
    fn from_generic(
        owner: &CoreRuntimeNet,
        dependency: CursorDependency<CoreSpecialization>,
    ) -> Self {
        match dependency {
            CursorDependency::LocalCursor(cursor) => Self::LocalCursor(cursor),
            CursorDependency::SourceCursor(observation) => {
                Self::SourceCursor(CoreFrontierObservation::from_generic(owner, observation))
            }
            CursorDependency::SourceFrontier(observation) => {
                Self::SourceFrontier(CoreFrontierObservation::from_generic(owner, observation))
            }
        }
    }

    fn to_generic(&self) -> CursorDependency<CoreSpecialization> {
        match self {
            Self::LocalCursor(cursor) => CursorDependency::LocalCursor(*cursor),
            Self::SourceCursor(observation) => {
                CursorDependency::SourceCursor(observation.inner.clone())
            }
            Self::SourceFrontier(observation) => {
                CursorDependency::SourceFrontier(observation.inner.clone())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CoreCursorStep {
    Progressed(CursorProgress),
    Dependency(CoreCursorDependency),
    Stable,
    Contended(CoreNetContention),
    Disturbed,
    Gone,
}

impl CoreCursorStep {
    fn from_generic(owner: &CoreRuntimeNet, step: CursorStep<CoreSpecialization>) -> Self {
        match step {
            CursorStep::Progressed(progress) => Self::Progressed(progress),
            CursorStep::Dependency(dependency) => {
                Self::Dependency(CoreCursorDependency::from_generic(owner, dependency))
            }
            CursorStep::Stable => Self::Stable,
            CursorStep::Contended(contention) => {
                Self::Contended(CoreNetContention::new(owner, contention))
            }
            CursorStep::Disturbed => Self::Disturbed,
            CursorStep::Gone => Self::Gone,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CoreActivePairStep {
    Reduction(Reduction),
    Cursor(NodeId),
    BlockedCall(BlockedCall<CoreWaitToken>),
    BlockedOperatorCall(BlockedOperatorCall<CoreWaitToken>),
    Stuck,
    Contended(CoreNetContention),
    Disturbed,
    Gone,
}

impl CoreActivePairStep {
    fn from_generic(owner: &CoreRuntimeNet, step: ActivePairStep<CoreSpecialization>) -> Self {
        match step {
            ActivePairStep::Reduction(reduction) => Self::Reduction(reduction),
            ActivePairStep::Cursor(cursor) => Self::Cursor(cursor),
            ActivePairStep::BlockedCall(blocked) => Self::BlockedCall(blocked),
            ActivePairStep::BlockedOperatorCall(blocked) => Self::BlockedOperatorCall(blocked),
            ActivePairStep::Stuck(_) => Self::Stuck,
            ActivePairStep::Contended(contention) => {
                Self::Contended(CoreNetContention::new(owner, contention))
            }
            ActivePairStep::Disturbed => Self::Disturbed,
            ActivePairStep::Gone => Self::Gone,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    macro_rules! assert_does_not_implement {
        ($module:ident, $type:ty, $trait:path) => {
            mod $module {
                use super::*;

                trait AmbiguousIfImplemented<Discriminator> {
                    fn verify() {}
                }

                struct Implemented;

                impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
                impl<T: ?Sized + $trait> AmbiguousIfImplemented<Implemented> for T {}

                const _: fn() = || {
                    <$type as AmbiguousIfImplemented<_>>::verify();
                };
            }
        };
    }

    assert_does_not_implement!(
        core_runtime_net_access_is_not_send,
        CoreRuntimeNetAccess<'static, 'static>,
        Send
    );
    assert_does_not_implement!(
        core_runtime_net_access_is_not_sync,
        CoreRuntimeNetAccess<'static, 'static>,
        Sync
    );

    fn closed_unit_template(values: &CoreValueFactory) -> CoreInteractionNet {
        let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let data = builder.data(values.unit());
        builder.finish(data)
    }

    #[test]
    fn core_net_provenance_distinguishes_runtimes() {
        let first = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let second = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&first);
        let net = first.instantiate_core_net(&template);

        assert!(net.belongs_to(&first));
        assert!(!net.belongs_to(&second));
    }

    #[test]
    #[should_panic(expected = "but access came from evaluation runtime")]
    fn core_net_access_rejects_a_foreign_runtime() {
        let owner = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let foreign = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&owner);
        let net = owner.instantiate_core_net(&template);

        foreign.with_runtime_value_access(|access| {
            let _ = net.access(&access);
        });
    }

    #[test]
    fn identity_only_net_work_outlives_scoped_access() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&values);
        let net = values.instantiate_core_net(&template);
        let alias = net.clone();

        values.with_runtime_value_access(|values| {
            let access = net.access(&values);
            let exposed = access.with(RuntimeNet::exposed);
            assert!(access.with(|runtime| runtime.interface_data(exposed).is_some()));
        });

        assert!(net.ptr_eq(&alias));
    }

    #[test]
    #[should_panic(expected = "a core net cannot copy topology from another value domain")]
    fn core_copy_source_rejects_a_foreign_runtime() {
        let first = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let second = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let first_template = closed_unit_template(&first);
        let second_template = closed_unit_template(&second);
        let source = first
            .instantiate_core_net(&first_template)
            .test_prepare_copy_source();
        let target = second.instantiate_core_net(&second_template);

        let _ = source.into_inner_for(&target.values);
    }

    #[test]
    fn core_net_provenance_does_not_retain_the_value_domain() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let domain = Arc::downgrade(values.value_domain());
        let template = closed_unit_template(&values);
        let net = values.instantiate_core_net(&template);

        assert!(net.domain_is_live());
        drop(values);
        assert!(domain.upgrade().is_none());
        assert!(!net.domain_is_live());
    }

    #[test]
    fn raw_core_runtime_net_construction_is_confined_to_its_facade() {
        fn visit(root: &Path, path: PathBuf) {
            for entry in fs::read_dir(&path).expect("source directory must be readable") {
                let entry = entry.expect("source entry must be readable");
                let path = entry.path();
                if path.is_dir() {
                    visit(root, path);
                    continue;
                }
                if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(root)
                    .expect("visited source must remain below source root");
                if relative == Path::new("core_net.rs")
                    || relative == Path::new("interaction_net.rs")
                    || relative.starts_with("interaction_net")
                {
                    continue;
                }
                let source = fs::read_to_string(&path).expect("Rust source must be readable");
                assert!(
                    !source.contains(".instantiate_shared()"),
                    "{} constructs a raw shared interaction net outside the generic runtime or core facade",
                    relative.display()
                );
                assert!(
                    !source.contains("SharedRuntimeNet<CoreSpecialization>"),
                    "{} names the raw core shared-net owner outside its facade",
                    relative.display()
                );
            }
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        visit(&root, root.clone());
    }

    #[test]
    fn durable_core_net_facade_has_no_ordinary_inspection_surface() {
        let source = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("core_net.rs"),
        )
        .expect("core-net source must be readable");
        let facade = source
            .split_once("impl CoreRuntimeNet {")
            .expect("core-net facade implementation must remain present")
            .1
            .split_once("impl CoreRuntimeNetAccess<'_, '_> {")
            .expect("scoped core-net access implementation must remain present")
            .0;

        for forbidden in [
            "pub(crate) fn with<",
            "pub(crate) fn with_mut<",
            "pub(crate) fn poll_interface_demand(",
            "pub(crate) fn resolve_cursor_dependency(",
            "pub(crate) fn step_cursor(",
            "pub(crate) fn step_active_pair(",
            "pub(crate) fn advance_claimed_cursor(",
            "pub(crate) fn prepare_copy_source(",
            "pub(crate) fn resume_claimed_call_with_copy(",
        ] {
            assert!(
                !facade.contains(forbidden),
                "durable core-net facade regained authority-free operation {forbidden}"
            );
        }
    }
}
