//! Crate-private protocol for resumable evaluation to outer WHNF.
//!
//! W1A installs the state vocabulary and W1B adds its callback-free regional
//! driver. W1C projects and publishes durable checkpoints at real regional
//! boundaries. W2 uses that protocol for client demand and promise following;
//! later checkpoints extend it through lazy sources and caller frames.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::core::{
    CoreValueFactory, DeferredValueId, EvaluationFailure, FunctionValue, LazyValue,
    ManagedLazyRoot, ManagedPromiseRoot, PromisedValue, Value,
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
        result: Option<RuntimeValueRoot>,
    },
    Demand(DurableWhnfState),
}

/// Machine-safe state retained whenever regional managed access is closed.
///
/// Every semantic value in this type is represented by an exact runtime root.
/// Region-bound raw values exist only in [`RegionalWhnfWork`].
pub(crate) struct DurableWhnfState {
    focus: RuntimeValueRoot,
    frames: Vec<DurableWhnfContinuation>,
    followed: BTreeSet<DeferredValueId>,
}

/// One suspended caller frame with all cross-boundary semantic values rooted.
pub(crate) struct DurableWhnfFrame {
    kind: WhnfFrameKind,
    cursor: usize,
    retained: Vec<RuntimeValueRoot>,
}

enum DurableWhnfContinuation {
    Generic(DurableWhnfFrame),
    Application {
        arguments: Vec<RuntimeValueRoot>,
        next: usize,
    },
    DictionaryApplication {
        effect_payload: RuntimeValueRoot,
        remaining_effect_values: Vec<RuntimeValueRoot>,
        next_effect_value: usize,
        apply_member: Option<RuntimeValueRoot>,
    },
    SemanticUndefined {
        purpose: UndefinedPurpose,
        ancestors: Vec<DurableUndefinedDictionary>,
        phase: UndefinedPhase,
    },
    StaticAccess {
        keys: Arc<[crate::core::Key]>,
        next: usize,
    },
}

struct DurableUndefinedDictionary {
    members: Vec<RuntimeValueRoot>,
    next: usize,
}

/// Callback-free working state projected beneath one managed-access region.
pub(crate) struct RegionalWhnfWork {
    focus: Value,
    frames: Vec<RegionalWhnfContinuation>,
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

enum RegionalWhnfContinuation {
    Generic(RegionalWhnfFrame),
    Application {
        arguments: Vec<Value>,
        next: usize,
    },
    DictionaryApplication {
        effect_payload: Value,
        remaining_effect_values: Vec<Value>,
        next_effect_value: usize,
        apply_member: Option<Value>,
    },
    SemanticUndefined {
        purpose: UndefinedPurpose,
        ancestors: Vec<RegionalUndefinedDictionary>,
        phase: UndefinedPhase,
    },
    StaticAccess {
        keys: Arc<[crate::core::Key]>,
        next: usize,
    },
}

struct RegionalUndefinedDictionary {
    members: Vec<Value>,
    next: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UndefinedPurpose {
    EffectPayload,
    EffectExtra,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UndefinedPhase {
    Inspect,
    ReturnTrue,
}

impl From<RegionalWhnfFrame> for RegionalWhnfContinuation {
    fn from(frame: RegionalWhnfFrame) -> Self {
        Self::Generic(frame)
    }
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
                .map(|frame| DurableWhnfContinuation::root_continuation(access, frame))
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

impl DurableWhnfContinuation {
    fn project(&self, access: &EvaluationValueAccess<'_>) -> RegionalWhnfContinuation {
        match self {
            Self::Generic(frame) => RegionalWhnfContinuation::Generic(frame.project(access)),
            Self::Application { arguments, next } => RegionalWhnfContinuation::Application {
                arguments: arguments
                    .iter()
                    .map(|argument| access.clone_root(argument))
                    .collect(),
                next: *next,
            },
            Self::DictionaryApplication {
                effect_payload,
                remaining_effect_values,
                next_effect_value,
                apply_member,
            } => RegionalWhnfContinuation::DictionaryApplication {
                effect_payload: access.clone_root(effect_payload),
                remaining_effect_values: remaining_effect_values
                    .iter()
                    .map(|value| access.clone_root(value))
                    .collect(),
                next_effect_value: *next_effect_value,
                apply_member: apply_member.as_ref().map(|value| access.clone_root(value)),
            },
            Self::SemanticUndefined {
                purpose,
                ancestors,
                phase,
            } => RegionalWhnfContinuation::SemanticUndefined {
                purpose: *purpose,
                ancestors: ancestors
                    .iter()
                    .map(|ancestor| RegionalUndefinedDictionary {
                        members: ancestor
                            .members
                            .iter()
                            .map(|value| access.clone_root(value))
                            .collect(),
                        next: ancestor.next,
                    })
                    .collect(),
                phase: *phase,
            },
            Self::StaticAccess { keys, next } => RegionalWhnfContinuation::StaticAccess {
                keys: Arc::clone(keys),
                next: *next,
            },
        }
    }

    fn root_continuation(
        access: &EvaluationValueAccess<'_>,
        frame: RegionalWhnfContinuation,
    ) -> Self {
        match frame {
            RegionalWhnfContinuation::Generic(frame) => {
                Self::Generic(DurableWhnfFrame::root_regional(access, frame))
            }
            RegionalWhnfContinuation::Application { arguments, next } => Self::Application {
                arguments: arguments
                    .into_iter()
                    .map(|argument| access.values().root_runtime_value(argument))
                    .collect(),
                next,
            },
            RegionalWhnfContinuation::DictionaryApplication {
                effect_payload,
                remaining_effect_values,
                next_effect_value,
                apply_member,
            } => Self::DictionaryApplication {
                effect_payload: access.values().root_runtime_value(effect_payload),
                remaining_effect_values: remaining_effect_values
                    .into_iter()
                    .map(|value| access.values().root_runtime_value(value))
                    .collect(),
                next_effect_value,
                apply_member: apply_member.map(|value| access.values().root_runtime_value(value)),
            },
            RegionalWhnfContinuation::SemanticUndefined {
                purpose,
                ancestors,
                phase,
            } => Self::SemanticUndefined {
                purpose,
                ancestors: ancestors
                    .into_iter()
                    .map(|ancestor| DurableUndefinedDictionary {
                        members: ancestor
                            .members
                            .into_iter()
                            .map(|value| access.values().root_runtime_value(value))
                            .collect(),
                        next: ancestor.next,
                    })
                    .collect(),
                phase,
            },
            RegionalWhnfContinuation::StaticAccess { keys, next } => {
                Self::StaticAccess { keys, next }
            }
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
            checkpoint: DurableWhnfCheckpoint::Source {
                lazy,
                runtime,
                result: None,
            },
        }
    }

    pub(crate) fn source_root(&self) -> Option<&ManagedLazyRoot> {
        let DurableWhnfCheckpoint::Source { lazy, result, .. } = &self.checkpoint else {
            return None;
        };
        result.is_none().then_some(lazy)
    }

    pub(crate) fn install_source_result(&mut self, focus: RuntimeValueRoot) {
        let DurableWhnfCheckpoint::Source {
            runtime, result, ..
        } = &mut self.checkpoint
        else {
            panic!("a lazy source result may be installed only once")
        };
        assert_eq!(
            *runtime,
            focus.runtime_id(),
            "a lazy source result must belong to its producer runtime"
        );
        *result = Some(focus);
    }

    pub(crate) fn source_result(&self) -> Option<&RuntimeValueRoot> {
        let DurableWhnfCheckpoint::Source { result, .. } = &self.checkpoint else {
            return None;
        };
        result.as_ref()
    }

    pub(crate) fn from_application_checkpoint_in(
        access: &EvaluationValueAccess<'_>,
        function: Value,
        arguments: &[Value],
    ) -> Self {
        assert!(!arguments.is_empty(), "application requires an argument");
        Self {
            checkpoint: DurableWhnfCheckpoint::Demand(DurableWhnfState {
                focus: access.values().root_runtime_value(function),
                frames: vec![DurableWhnfContinuation::Application {
                    arguments: arguments
                        .iter()
                        .map(|argument| {
                            access
                                .values()
                                .root_runtime_value(access.values().duplicate_value(argument))
                        })
                        .collect(),
                    next: 0,
                }],
                followed: BTreeSet::new(),
            }),
        }
    }

    pub(crate) fn from_static_access_checkpoint_in(
        access: &EvaluationValueAccess<'_>,
        base: Value,
        keys: Arc<[crate::core::Key]>,
    ) -> Self {
        let frames = (!keys.is_empty())
            .then_some(DurableWhnfContinuation::StaticAccess { keys, next: 0 })
            .into_iter()
            .collect();
        Self {
            checkpoint: DurableWhnfCheckpoint::Demand(DurableWhnfState {
                focus: access.values().root_runtime_value(base),
                frames,
                followed: BTreeSet::new(),
            }),
        }
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
        _ if work.frames.is_empty() => {
            RegionalWhnfStep::Ready(access.values().duplicate_value(&work.focus))
        }
        _ => resume_semantic_frame(access, work),
    }
}

enum DirectApplicationStep {
    Applied { value: Value, consumed: usize },
    Dictionary(crate::core::Dict),
    Failed(Arc<EvaluationFailure>),
}

fn resume_semantic_frame(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfWork,
) -> RegionalWhnfStep {
    match work.frames.last() {
        Some(RegionalWhnfContinuation::SemanticUndefined { .. }) => {
            return resume_semantic_undefined(access, work);
        }
        Some(RegionalWhnfContinuation::Application { .. }) => {}
        Some(RegionalWhnfContinuation::DictionaryApplication { .. }) => {
            unreachable!("dictionary application must be evaluating an undefined candidate")
        }
        Some(RegionalWhnfContinuation::StaticAccess { .. }) => {
            return resume_static_access(access, work);
        }
        Some(RegionalWhnfContinuation::Generic(_)) => {
            unreachable!("W3C has not activated the remaining generic frame families")
        }
        None => unreachable!("a semantic frame resume requires one frame"),
    }

    let function = access.values().duplicate_value(&work.focus);
    let (arguments, next) = match work.frames.last() {
        Some(RegionalWhnfContinuation::Application { arguments, next }) => (arguments, *next),
        _ => unreachable!(),
    };
    let step = apply_whnf_callable(access, function, &arguments[next..]);
    match step {
        DirectApplicationStep::Applied { value, consumed } => {
            advance_application(access, work, value, consumed)
        }
        DirectApplicationStep::Dictionary(dict) => begin_dictionary_application(access, work, dict),
        DirectApplicationStep::Failed(failure) => RegionalWhnfStep::Failed(failure),
    }
}

fn resume_static_access(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfWork,
) -> RegionalWhnfStep {
    let frame = work
        .frames
        .pop()
        .expect("static access must retain its path frame");
    let RegionalWhnfContinuation::StaticAccess { keys, next } = frame else {
        unreachable!()
    };
    let Value::Dict(dict) = &work.focus else {
        return RegionalWhnfStep::Failed(Arc::new(EvaluationFailure::message(
            "value access base is not a dictionary",
        )));
    };
    let value = dict
        .get(&keys[next])
        .map(|value| access.values().duplicate_value(value))
        .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()));
    let next = next + 1;
    if next < keys.len() {
        work.frames
            .push(RegionalWhnfContinuation::StaticAccess { keys, next });
    }
    RegionalWhnfStep::Delegate(value)
}

fn advance_application(
    _access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfWork,
    value: Value,
    consumed: usize,
) -> RegionalWhnfStep {
    let Some(RegionalWhnfContinuation::Application { arguments, next }) = work.frames.last_mut()
    else {
        unreachable!("an application result requires its application frame")
    };
    *next += consumed;
    let complete = *next == arguments.len();
    if complete {
        work.frames.pop();
    }
    RegionalWhnfStep::Delegate(value)
}

fn begin_dictionary_application(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfWork,
    dict: crate::core::Dict,
) -> RegionalWhnfStep {
    let diagnostic_kind = if dict.is_empty() { "Undefined" } else { "Dict" };
    let apply_member = dict
        .get(&*crate::core::keys::APPLY)
        .map(|value| access.values().duplicate_value(value));
    let Some(effect_payload) = dict
        .get(&*crate::core::keys::EFF)
        .map(|value| access.values().duplicate_value(value))
    else {
        return dictionary_application_fallback(access, apply_member, diagnostic_kind);
    };
    let remaining_effect_values = dict
        .iter()
        .filter(|(key, _)| *key != &*crate::core::keys::EFF)
        .map(|(_, value)| access.values().duplicate_value(value))
        .collect();
    let candidate = access.values().duplicate_value(&effect_payload);
    work.frames
        .push(RegionalWhnfContinuation::DictionaryApplication {
            effect_payload,
            remaining_effect_values,
            next_effect_value: 0,
            apply_member,
        });
    begin_semantic_undefined(access, work, candidate, UndefinedPurpose::EffectPayload)
}

fn begin_semantic_undefined(
    _access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfWork,
    candidate: Value,
    purpose: UndefinedPurpose,
) -> RegionalWhnfStep {
    work.frames
        .push(RegionalWhnfContinuation::SemanticUndefined {
            purpose,
            ancestors: Vec::new(),
            phase: UndefinedPhase::Inspect,
        });
    RegionalWhnfStep::Delegate(candidate)
}

fn resume_semantic_undefined(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfWork,
) -> RegionalWhnfStep {
    let frame = work
        .frames
        .pop()
        .expect("semantic-undefined work must retain its frame");
    let RegionalWhnfContinuation::SemanticUndefined {
        purpose,
        mut ancestors,
        phase,
    } = frame
    else {
        unreachable!()
    };
    match phase {
        UndefinedPhase::Inspect => {
            let retained_focus = access.values().duplicate_value(&work.focus);
            let Value::Dict(dict) = &retained_focus else {
                return finish_semantic_undefined(access, work, purpose, false);
            };
            let members = dict
                .iter()
                .map(|(_, value)| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            let Some(first) = members.first() else {
                work.frames
                    .push(RegionalWhnfContinuation::SemanticUndefined {
                        purpose,
                        ancestors,
                        phase: UndefinedPhase::ReturnTrue,
                    });
                return RegionalWhnfStep::Delegate(retained_focus);
            };
            let first = access.values().duplicate_value(first);
            ancestors.push(RegionalUndefinedDictionary { members, next: 1 });
            work.frames
                .push(RegionalWhnfContinuation::SemanticUndefined {
                    purpose,
                    ancestors,
                    phase: UndefinedPhase::Inspect,
                });
            RegionalWhnfStep::Delegate(first)
        }
        UndefinedPhase::ReturnTrue => {
            let retained_focus = access.values().duplicate_value(&work.focus);
            let Some(ancestor) = ancestors.last_mut() else {
                return finish_semantic_undefined(access, work, purpose, true);
            };
            if ancestor.next < ancestor.members.len() {
                let next = access
                    .values()
                    .duplicate_value(&ancestor.members[ancestor.next]);
                ancestor.next += 1;
                work.frames
                    .push(RegionalWhnfContinuation::SemanticUndefined {
                        purpose,
                        ancestors,
                        phase: UndefinedPhase::Inspect,
                    });
                RegionalWhnfStep::Delegate(next)
            } else {
                ancestors.pop();
                work.frames
                    .push(RegionalWhnfContinuation::SemanticUndefined {
                        purpose,
                        ancestors,
                        phase: UndefinedPhase::ReturnTrue,
                    });
                RegionalWhnfStep::Delegate(retained_focus)
            }
        }
    }
}

fn finish_semantic_undefined(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfWork,
    purpose: UndefinedPurpose,
    undefined: bool,
) -> RegionalWhnfStep {
    let frame = work
        .frames
        .pop()
        .expect("dictionary application must underlie its undefined walk");
    let RegionalWhnfContinuation::DictionaryApplication {
        effect_payload,
        remaining_effect_values,
        mut next_effect_value,
        apply_member,
    } = frame
    else {
        unreachable!("semantic-undefined result must return to dictionary application")
    };

    let effect_rejected = match purpose {
        UndefinedPurpose::EffectPayload => undefined,
        UndefinedPurpose::EffectExtra => !undefined,
    };
    if effect_rejected {
        return dictionary_application_fallback(access, apply_member, "Dict");
    }
    if next_effect_value < remaining_effect_values.len() {
        let candidate = access
            .values()
            .duplicate_value(&remaining_effect_values[next_effect_value]);
        next_effect_value += 1;
        work.frames
            .push(RegionalWhnfContinuation::DictionaryApplication {
                effect_payload,
                remaining_effect_values,
                next_effect_value,
                apply_member,
            });
        return begin_semantic_undefined(access, work, candidate, UndefinedPurpose::EffectExtra);
    }

    let argument = match work.frames.last() {
        Some(RegionalWhnfContinuation::Application { arguments, next }) => {
            access.values().duplicate_value(&arguments[*next])
        }
        _ => unreachable!("dictionary application must retain its caller frame"),
    };
    let effect = super::application::effect_value(super::application::apply_effect_function_value(
        effect_payload,
        argument,
    ));
    advance_application(access, work, effect, 1)
}

fn dictionary_application_fallback(
    access: &EvaluationValueAccess<'_>,
    apply_member: Option<Value>,
    diagnostic_kind: &str,
) -> RegionalWhnfStep {
    if let Some(apply_member) = apply_member
        && !super::value::is_undefined_dict_value(access.values(), &apply_member)
    {
        return RegionalWhnfStep::Delegate(apply_member);
    }
    RegionalWhnfStep::Failed(Arc::new(EvaluationFailure::message(format!(
        "application requires a function value, received {diagnostic_kind}"
    ))))
}

fn apply_whnf_callable(
    access: &EvaluationValueAccess<'_>,
    function: Value,
    arguments: &[Value],
) -> DirectApplicationStep {
    debug_assert!(!arguments.is_empty());
    match function {
        Value::Builtin(builtin) => apply_whnf_builtin(access, builtin, Vec::new(), arguments),
        Value::PartialBuiltin(call) => apply_whnf_builtin(
            access,
            call.builtin,
            call.arguments
                .iter()
                .map(|argument| access.values().duplicate_value(argument))
                .collect(),
            arguments,
        ),
        Value::Function(function) => apply_whnf_function(access, function, arguments),
        Value::Dict(dict) => DirectApplicationStep::Dictionary(dict),
        value => DirectApplicationStep::Failed(Arc::new(EvaluationFailure::message(format!(
            "application requires a function value, received {}",
            value.diagnostic_kind_name()
        )))),
    }
}

fn apply_whnf_builtin(
    access: &EvaluationValueAccess<'_>,
    builtin: crate::core::Builtin,
    mut supplied: Vec<Value>,
    arguments: &[Value],
) -> DirectApplicationStep {
    let remaining = builtin
        .arity()
        .checked_sub(supplied.len())
        .expect("a partial builtin cannot contain too many arguments");
    let consumed = remaining.min(arguments.len());
    supplied.extend(
        arguments[..consumed]
            .iter()
            .map(|argument| access.values().duplicate_value(argument)),
    );
    DirectApplicationStep::Applied {
        value: Value::builtin_call_in(access.values(), builtin, supplied),
        consumed,
    }
}

fn apply_whnf_function(
    access: &EvaluationValueAccess<'_>,
    function: FunctionValue,
    arguments: &[Value],
) -> DirectApplicationStep {
    let remaining = function.remaining_arity();
    let consumed = remaining.min(arguments.len());
    let saturating = arguments[..consumed]
        .iter()
        .map(|argument| access.values().duplicate_value(argument))
        .collect::<Vec<_>>();
    let value = if consumed < remaining {
        let stage = function.duplicate_stage_in(access.values());
        Value::Function(FunctionValue::new(
            super::net::attach_net_many_in(access.values(), stage, saturating),
            remaining - consumed,
        ))
    } else {
        Value::Lazy(LazyValue::from_function_call_in(
            access.values(),
            function,
            Arc::from(saturating),
        ))
    };
    DirectApplicationStep::Applied { value, consumed }
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

#[cfg(test)]
#[path = "whnf/tests/w3b_application.rs"]
mod w3b_application_tests;

#[cfg(test)]
#[path = "whnf/tests/w3c_access.rs"]
mod w3c_access_tests;
