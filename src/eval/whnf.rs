//! Crate-private protocol for resumable evaluation to outer WHNF.
//!
//! W1A installs the state vocabulary and W1B adds its callback-free regional
//! driver. W1C projects and publishes durable checkpoints at real regional
//! boundaries. W2 uses that protocol for client demand and promise following;
//! later checkpoints extend it through lazy sources and caller frames.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::core::{
    CoreValueFactory, DeferredValueId, EvaluationFailure, ManagedLazyRoot, ManagedPromiseRoot,
    PromisedValue, Value,
};
use crate::core_net::CoreWaitToken;
use crate::evaluation::EvaluationValueAccess;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

/// One resumable request to reduce a value's outer deferred shells to WHNF.
///
/// The checkpoint is intentionally neither cloneable nor publicly exposed.
/// An outer owner moves one computation between polls and remains solely
/// responsible for the eventual result destination.
pub(crate) struct WhnfComputation {
    checkpoint: DurableWhnfCheckpoint,
}

/// Durable entry mode for one WHNF request.
///
/// A lazy producer begins with the exact lazy identity whose source it owns.
/// Once that source has produced a value, the same computation installs the
/// ordinary rooted demand checkpoint and never reconstructs the source result.
enum DurableWhnfCheckpoint {
    Source {
        lazy: ManagedLazyRoot,
        runtime: crate::runtime::EvaluationRuntimeId,
    },
    Demand(DurableWhnfState),
}

/// Machine-safe state retained whenever regional managed access is closed.
///
/// Every semantic value in this type is represented by an exact runtime root.
/// Region-bound raw values exist only in [`RegionalWhnfWork`].
pub(crate) struct DurableWhnfState {
    focus: RuntimeValueRoot,
    frames: Vec<DurableWhnfFrame>,
    followed: BTreeSet<DeferredValueId>,
}

/// One suspended caller frame with all cross-boundary semantic values rooted.
pub(crate) struct DurableWhnfFrame {
    kind: WhnfFrameKind,
    cursor: usize,
    retained: Vec<RuntimeValueRoot>,
}

/// Callback-free working state projected beneath one managed-access region.
pub(crate) struct RegionalWhnfWork {
    focus: Value,
    frames: Vec<RegionalWhnfFrame>,
    followed: BTreeSet<DeferredValueId>,
}

/// Regional counterpart of [`DurableWhnfFrame`].
///
/// These raw values may be copied and rearranged only while the evaluator's
/// matching mutator remains active. This type never enters a machine field.
pub(crate) struct RegionalWhnfFrame {
    kind: WhnfFrameKind,
    cursor: usize,
    retained: Vec<Value>,
}

/// Shared resumption shapes selected by the W0 census.
#[allow(
    dead_code,
    reason = "W0 selected the complete frame vocabulary; W3-W6 construct the deeper frame families"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WhnfFrameKind {
    DemandThenInspect,
    OrderedOperands,
    CollectionWalk,
    Application,
    KeyConversion,
    AccessPath,
    DiagnosticContext,
}

/// One callback-free regional evaluator transition.
///
/// `Delegate` replaces the current focus without pushing a frame. `Boundary`
/// carries only a durable request; its interpretation belongs to the outer
/// evaluation driver after regional access closes.
#[allow(
    dead_code,
    reason = "W2 needs delegation and terminal steps; W3+ adds explicit continuation frames"
)]
pub(crate) enum RegionalWhnfStep {
    Delegate(Value),
    Continue(RegionalWhnfWork),
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Failed(Arc<EvaluationFailure>),
}

/// Outcome of driving callback-free WHNF work beneath one managed-access
/// region.
///
/// `Boundary` and `Yielded` deliberately return the exact regional work. W1C
/// will project that work into a durable checkpoint before the access region
/// closes; callers must not store this raw form in a machine.
pub(crate) enum RegionalWhnfDrive {
    Ready(Value),
    Boundary {
        work: RegionalWhnfWork,
        request: RegionalBoundaryRequest,
    },
    Yielded(RegionalWhnfWork),
    Failed(Arc<EvaluationFailure>),
}

/// Deterministic budget for one regional WHNF quantum.
pub(crate) struct WhnfStepBudget {
    remaining: usize,
}

impl WhnfStepBudget {
    pub(crate) fn new(steps: usize) -> Self {
        Self { remaining: steps }
    }

    fn consume(&mut self) -> bool {
        let Some(remaining) = self.remaining.checked_sub(1) else {
            return false;
        };
        self.remaining = remaining;
        true
    }

    #[cfg(test)]
    fn remaining(&self) -> usize {
        self.remaining
    }
}

/// Drives bounded callback-free transitions without recursive Rust calls.
///
/// The active access parameter is intentionally required even though the
/// driver itself only rearranges already-projected values. A reducer may copy
/// or inspect those values only through the same region. Dependency handling,
/// callbacks, root publication, and scheduler actions remain outside this
/// loop.
pub(crate) fn drive_regional<'scope>(
    access: &EvaluationValueAccess<'scope>,
    mut work: RegionalWhnfWork,
    budget: &mut WhnfStepBudget,
    mut reduce: impl FnMut(&EvaluationValueAccess<'scope>, &mut RegionalWhnfWork) -> RegionalWhnfStep,
) -> RegionalWhnfDrive {
    loop {
        if !budget.consume() {
            return RegionalWhnfDrive::Yielded(work);
        }
        match reduce(access, &mut work) {
            RegionalWhnfStep::Delegate(focus) => work.focus = focus,
            RegionalWhnfStep::Continue(next) => work = next,
            RegionalWhnfStep::Ready(value) => return RegionalWhnfDrive::Ready(value),
            RegionalWhnfStep::Boundary(request) => {
                return RegionalWhnfDrive::Boundary { work, request };
            }
            RegionalWhnfStep::Failed(failure) => return RegionalWhnfDrive::Failed(failure),
        }
    }
}

/// A regional result which requires orchestration outside managed access.
#[allow(
    dead_code,
    reason = "W2 implements deferred shells; W3-W4 construct direct dependencies and external boundaries"
)]
pub(crate) enum RegionalBoundaryRequest {
    Dependency(WhnfDependency),
    Deferred(WhnfDeferredRequest),
    External(WhnfExternalBoundary),
}

/// One unresolved semantic shell requiring policy outside managed access.
///
/// These registered roots preserve the exact lazy or promise identity. The
/// semantic reducer neither admits a producer nor subscribes to completion;
/// the durable owner performs those actions after its access region closes.
pub(crate) enum WhnfDeferredRequest {
    Lazy(ManagedLazyRoot),
    Promise(ManagedPromiseRoot),
    PromiseFollow(ManagedPromiseRoot),
}

/// External boundary family. Later checkpoints add the source-specific
/// durable payload only when a production boundary is migrated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "external source families are staged for W4")]
pub(crate) enum WhnfExternalBoundary {
    Reflection,
    Host,
    Net,
}

/// Exact completion source which can block a resumable WHNF computation.
///
/// This semantic/control type deliberately does not depend on the scheduler's
/// broader `WorkDependency` vocabulary. The evaluation boundary owns that
/// translation.
#[derive(Clone)]
#[allow(
    dead_code,
    reason = "W2 consumes translated dependencies; W3+ constructs them inside the semantic machine"
)]
pub(crate) enum WhnfDependency {
    Wait(CoreWaitToken),
    Promise(ManagedPromiseRoot),
}

/// Result of one bounded poll of a [`WhnfComputation`].
///
/// `Pending` and `Yielded` never consume the computation's exact checkpoint;
/// the outer owner polls the same instance again.
pub(crate) enum WhnfPoll {
    Ready(RuntimeValueRoot),
    Pending(WhnfDependency),
    Deferred(WhnfDeferredRequest),
    External(WhnfExternalBoundary),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl DurableWhnfState {
    fn project(&self, access: &EvaluationValueAccess<'_>) -> RegionalWhnfWork {
        RegionalWhnfWork {
            focus: access.clone_root(&self.focus),
            frames: self
                .frames
                .iter()
                .map(|frame| frame.project(access))
                .collect(),
            followed: self.followed.clone(),
        }
    }

    fn from_regional(access: &EvaluationValueAccess<'_>, work: RegionalWhnfWork) -> Self {
        Self {
            focus: access.values().root_runtime_value(work.focus),
            frames: work
                .frames
                .into_iter()
                .map(|frame| DurableWhnfFrame::root_regional(access, frame))
                .collect(),
            followed: work.followed,
        }
    }
}

impl DurableWhnfFrame {
    fn project(&self, access: &EvaluationValueAccess<'_>) -> RegionalWhnfFrame {
        RegionalWhnfFrame {
            kind: self.kind,
            cursor: self.cursor,
            retained: self
                .retained
                .iter()
                .map(|value| access.clone_root(value))
                .collect(),
        }
    }

    fn root_regional(access: &EvaluationValueAccess<'_>, frame: RegionalWhnfFrame) -> Self {
        Self {
            kind: frame.kind,
            cursor: frame.cursor,
            retained: frame
                .retained
                .into_iter()
                .map(|value| access.values().root_runtime_value(value))
                .collect(),
        }
    }
}

impl WhnfComputation {
    pub(crate) fn from_root(focus: RuntimeValueRoot) -> Self {
        Self {
            checkpoint: DurableWhnfCheckpoint::Demand(DurableWhnfState {
                focus,
                frames: Vec::new(),
                followed: BTreeSet::new(),
            }),
        }
    }

    pub(crate) fn from_lazy_source(
        lazy: ManagedLazyRoot,
        runtime: crate::runtime::EvaluationRuntimeId,
    ) -> Self {
        Self {
            checkpoint: DurableWhnfCheckpoint::Source { lazy, runtime },
        }
    }

    pub(crate) fn source_root(&self) -> Option<&ManagedLazyRoot> {
        let DurableWhnfCheckpoint::Source { lazy, .. } = &self.checkpoint else {
            return None;
        };
        Some(lazy)
    }

    pub(crate) fn install_source_result(&mut self, focus: RuntimeValueRoot) {
        let DurableWhnfCheckpoint::Source { runtime, .. } = &self.checkpoint else {
            panic!("a lazy source result may be installed only once")
        };
        assert_eq!(
            *runtime,
            focus.runtime_id(),
            "a lazy source result must belong to its producer runtime"
        );
        self.checkpoint = DurableWhnfCheckpoint::Demand(DurableWhnfState {
            focus,
            frames: Vec::new(),
            followed: BTreeSet::new(),
        });
    }

    pub(crate) fn from_promise_root(
        values: &CoreValueFactory,
        promise: &ManagedPromiseRoot,
    ) -> Self {
        let focus = values.construct_runtime_value_root(|access| {
            Value::Promised(PromisedValue::from_root(promise, access))
        });
        Self::from_root(focus)
    }

    pub(crate) fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId {
        match &self.checkpoint {
            DurableWhnfCheckpoint::Source { runtime, .. } => *runtime,
            DurableWhnfCheckpoint::Demand(checkpoint) => checkpoint.focus.runtime_id(),
        }
    }

    /// Polls one bounded callback-free quantum beneath matching value access.
    ///
    /// The prior checkpoint remains installed while its regional projection
    /// is driven and while a replacement is rooted. `publish_checkpoint`
    /// installs the complete replacement before retiring the old roots.
    /// Returned boundary dispositions contain no active access and are
    /// interpreted by the outer owner only after its access callback returns.
    pub(crate) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        budget: &mut WhnfStepBudget,
        reduce: impl FnMut(&EvaluationValueAccess<'_>, &mut RegionalWhnfWork) -> RegionalWhnfStep,
    ) -> WhnfPoll {
        let DurableWhnfCheckpoint::Demand(checkpoint) = &self.checkpoint else {
            panic!("a lazy source must install its result before WHNF demand")
        };
        let work = checkpoint.project(access);
        match drive_regional(access, work, budget, reduce) {
            RegionalWhnfDrive::Ready(value) => {
                WhnfPoll::Ready(access.values().root_runtime_value(value))
            }
            RegionalWhnfDrive::Boundary { work, request } => {
                self.publish_checkpoint(access, work);
                match request {
                    RegionalBoundaryRequest::Dependency(dependency) => {
                        WhnfPoll::Pending(dependency)
                    }
                    RegionalBoundaryRequest::Deferred(deferred) => WhnfPoll::Deferred(deferred),
                    RegionalBoundaryRequest::External(boundary) => WhnfPoll::External(boundary),
                }
            }
            RegionalWhnfDrive::Yielded(work) => {
                self.publish_checkpoint(access, work);
                WhnfPoll::Yielded
            }
            RegionalWhnfDrive::Failed(failure) => {
                WhnfPoll::Failed(access.values().root_runtime_failure(failure))
            }
        }
    }

    fn publish_checkpoint(&mut self, access: &EvaluationValueAccess<'_>, work: RegionalWhnfWork) {
        let replacement = DurableWhnfState::from_regional(access, work);
        let prior = std::mem::replace(
            &mut self.checkpoint,
            DurableWhnfCheckpoint::Demand(replacement),
        );
        drop(prior);
    }

    /// Polls the production outer-shell reducer beneath one managed region.
    ///
    /// W2 initially handles only lazy and promise shells. Later phases extend
    /// the same reducer with caller frames and source-specific work without
    /// changing the durable publication boundary.
    pub(crate) fn poll_semantic_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        budget: &mut WhnfStepBudget,
    ) -> WhnfPoll {
        self.poll_in(access, budget, reduce_semantic_shell)
    }
}

fn reduce_semantic_shell(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfWork,
) -> RegionalWhnfStep {
    debug_assert!(
        work.frames.is_empty(),
        "W2 outer-shell demand does not yet interpret caller frames"
    );
    match &work.focus {
        Value::Lazy(lazy) => match access.lazy(lazy).cached() {
            Some(Ok(value)) => {
                work.followed.insert(access.lazy(lazy).id().into());
                RegionalWhnfStep::Delegate(value.into_value())
            }
            Some(Err(failure)) => RegionalWhnfStep::Failed(failure),
            None => RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Deferred(
                WhnfDeferredRequest::Lazy(lazy.root_in(access.values())),
            )),
        },
        Value::Promised(promise) => match access.promise(promise).assignment() {
            Some(Ok(value)) => {
                if work.followed.insert(access.promise(promise).id().into()) {
                    RegionalWhnfStep::Delegate(value)
                } else {
                    RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Deferred(
                        WhnfDeferredRequest::PromiseFollow(promise.root_in(access.values())),
                    ))
                }
            }
            Some(Err(failure)) => RegionalWhnfStep::Failed(failure),
            None => RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Deferred(
                WhnfDeferredRequest::Promise(promise.root_in(access.values())),
            )),
        },
        _ => RegionalWhnfStep::Ready(access.values().duplicate_value(&work.focus)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

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

    assert_does_not_implement!(whnf_computation_is_not_clone, WhnfComputation, Clone);

    fn declaration<'source>(source: &'source str, start: &str, next: &str) -> &'source str {
        let start = source
            .find(start)
            .unwrap_or_else(|| panic!("missing declaration `{start}`"));
        let rest = &source[start..];
        let end = rest
            .find(next)
            .unwrap_or_else(|| panic!("missing declaration terminator `{next}`"));
        &rest[..end]
    }

    #[test]
    fn durable_state_contains_only_rooted_semantic_values() {
        let source = include_str!("whnf.rs");
        for declaration in [
            declaration(
                source,
                "pub(crate) struct DurableWhnfState",
                "/// One suspended caller frame",
            ),
            declaration(
                source,
                "pub(crate) struct DurableWhnfFrame",
                "/// Callback-free working state",
            ),
        ] {
            for forbidden in [
                "focus: Value",
                "Vec<Value>",
                "RuntimeValueAccess",
                "EvaluationValueAccess",
                "EvaluatorStepContext",
            ] {
                assert!(
                    !declaration.contains(forbidden),
                    "durable WHNF state must not contain `{forbidden}`"
                );
            }
        }
    }

    #[test]
    fn whnf_protocol_remains_private_and_has_only_named_production_owners() {
        let source = include_str!("whnf.rs");
        for declaration in [
            "pub(crate) struct WhnfComputation",
            "pub(crate) enum WhnfFrameKind",
            "pub(crate) enum WhnfDependency",
            "pub(crate) enum WhnfPoll",
        ] {
            assert!(
                source.contains(declaration),
                "WHNF protocol declaration must remain crate-private: {declaration}"
            );
        }
        let task_machine_impl = ["impl EvaluationTaskMachine", " for WhnfComputation"].concat();
        assert!(!source.contains(&task_machine_impl));

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let core = fs::read_to_string(manifest.join("src/core.rs"))
            .expect("core value source should be readable");
        assert!(
            !core.contains("WhnfComputation"),
            "WHNF progress must not become a Value or LazySource variant"
        );
        let deferred = fs::read_to_string(manifest.join("src/eval/value.rs"))
            .expect("deferred evaluator source should be readable");
        assert!(deferred.contains("computation: super::whnf::WhnfComputation"));
        let client =
            fs::read_to_string(manifest.join("src/evaluation/coordinator/client_demand.rs"))
                .expect("client-demand source should be readable");
        assert!(
            client.contains("ClientDemandOperation(pub(in crate::evaluation) WhnfComputation)")
        );

        let reflection = fs::read_to_string(manifest.join("src/reflection/machine.rs"))
            .expect("reflection machine source should be readable");
        assert!(
            !reflection.contains("WhnfComputation"),
            "reflection cutover remains staged for W5"
        );
    }

    #[test]
    fn selected_frame_protocol_is_compile_exhaustive() {
        fn classify(kind: WhnfFrameKind) -> usize {
            match kind {
                WhnfFrameKind::DemandThenInspect => 0,
                WhnfFrameKind::OrderedOperands => 1,
                WhnfFrameKind::CollectionWalk => 2,
                WhnfFrameKind::Application => 3,
                WhnfFrameKind::KeyConversion => 4,
                WhnfFrameKind::AccessPath => 5,
                WhnfFrameKind::DiagnosticContext => 6,
            }
        }

        assert_eq!(classify(WhnfFrameKind::DiagnosticContext), 6);
    }
}

#[cfg(test)]
#[path = "whnf/tests/w1b.rs"]
mod w1b_tests;

#[cfg(test)]
#[path = "whnf/tests/w1c.rs"]
mod w1c_tests;

#[cfg(test)]
#[path = "whnf/tests/w2a.rs"]
mod w2a_tests;

#[cfg(test)]
#[path = "whnf/tests/w2b.rs"]
mod w2b_tests;
