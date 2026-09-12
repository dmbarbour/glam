//! Crate-private protocol for resumable evaluation to outer WHNF.
//!
//! W1A installs only the state vocabulary. Regional reduction and checkpoint
//! projection arrive in W1B-W1C; production evaluator entry points remain on
//! their existing path until their named migration checkpoints.

#![allow(
    dead_code,
    reason = "W1A installs the additive WHNF protocol before W1B-W1C implement and exercise it"
)]

use std::sync::Arc;

use crate::core::{EvaluationFailure, ManagedPromiseRoot, Value};
use crate::core_net::CoreWaitToken;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

/// One resumable request to reduce a value's outer deferred shells to WHNF.
///
/// The checkpoint is intentionally neither cloneable nor publicly exposed.
/// An outer owner moves one computation between polls and remains solely
/// responsible for the eventual result destination.
pub(crate) struct WhnfComputation {
    checkpoint: DurableWhnfState,
}

/// Machine-safe state retained whenever regional managed access is closed.
///
/// Every semantic value in this type is represented by an exact runtime root.
/// Region-bound raw values exist only in [`RegionalWhnfWork`].
pub(crate) struct DurableWhnfState {
    focus: RuntimeValueRoot,
    frames: Vec<DurableWhnfFrame>,
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
pub(crate) enum RegionalWhnfStep {
    Delegate(Value),
    Continue(RegionalWhnfWork),
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Failed(Arc<EvaluationFailure>),
}

/// A regional result which requires orchestration outside managed access.
pub(crate) enum RegionalBoundaryRequest {
    Dependency(WhnfDependency),
    External(WhnfExternalBoundary),
}

/// External boundary family. Later checkpoints add the source-specific
/// durable payload only when a production boundary is migrated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl WhnfComputation {
    pub(crate) fn from_root(focus: RuntimeValueRoot) -> Self {
        Self {
            checkpoint: DurableWhnfState {
                focus,
                frames: Vec::new(),
            },
        }
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
    fn whnf_protocol_remains_private_and_outside_semantic_values() {
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
        for entry in [
            "src/eval/value.rs",
            "src/evaluation/coordinator/client_demand.rs",
            "src/reflection/machine.rs",
        ] {
            let source = fs::read_to_string(manifest.join(entry))
                .expect("production entry source should be readable");
            assert!(
                !source.contains("WhnfComputation"),
                "W1A must not cut over production entry point {entry}"
            );
        }
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
