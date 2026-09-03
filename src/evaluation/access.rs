//! Scoped managed-value authority for evaluator substeps.
//!
//! A poll context carries no active mutator. It may open a
//! bounded callback-free evaluator region, whose lifetime-bound authority
//! cannot enter durable machine state or cross a thread. Production machine,
//! client-demand, spark, direct-effect, and isolated-search polls receive this
//! context; I3B-I3D partition the opaque evaluator operations which may safely
//! open it.

use crate::core::{EvaluationFailure, RuntimeValueAccess, Value};
use crate::core_net::{CoreRuntimeNet, CoreRuntimeNetAccess};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::Ordering;

use super::coordinator::ClaimedDemandSession;
use super::{EvalContext, EvaluationDemandState, ReflectionTaskReservation};

/// A matching-runtime evaluator view over one active managed-access region.
pub(crate) struct EvaluationValueAccess<'scope> {
    values: RuntimeValueAccess<'scope>,
}

/// Thread-bound authority for one evaluator orchestration step.
///
/// Unlike [`EvaluationValueAccess`], this carrier contains no active mutator
/// and may remain live while evaluation reports a dependency or invokes a
/// callback. Callback-free semantic operations use [`Self::with_value_access`]
/// to open smaller managed-access regions. Its private construction preserves
/// the poll admission route established by I3A and completed by I3C.
pub(crate) struct EvaluatorStepContext<'step> {
    admission: EvaluatorStepAdmission<'step>,
    context: &'step EvalContext,
    pending_reflection_activations: RefCell<Vec<ReflectionTaskReservation>>,
    _thread_bound: PhantomData<Rc<()>>,
}

enum EvaluatorStepAdmission<'step> {
    Poll(&'step EvaluationPollContext),
    /// Temporary direct entry for I3B.1c builtin seams and the
    /// source-inventoried I3D/I3E callers.
    ///
    /// This route does not keep a mutator active. It exists only until those
    /// callers receive their scheduler- or runtime-service-owned authority.
    DirectCompatibility,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct ValueAccessDomainMismatch;

impl<'scope> EvaluationValueAccess<'scope> {
    fn try_new(
        context: &'scope EvalContext,
        values: RuntimeValueAccess<'scope>,
    ) -> Result<Self, ValueAccessDomainMismatch> {
        if !values.belongs_to(context.values()) {
            return Err(ValueAccessDomainMismatch);
        }
        Ok(Self { values })
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> &RuntimeValueAccess<'scope> {
        &self.values
    }

    /// Derives bounded access to one net owned by this value domain.
    pub(crate) fn net<'access>(
        &'access self,
        runtime: &'access CoreRuntimeNet,
    ) -> CoreRuntimeNetAccess<'access, 'scope> {
        runtime.access(&self.values)
    }

    /// Clones one compatibility root only while matching value-domain access
    /// is admitted for this evaluator step.
    pub(crate) fn clone_root(&self, root: &RuntimeValueRoot) -> Value {
        root.clone_core_with(&self.values)
    }
}

impl EvaluatorStepContext<'_> {
    pub(crate) fn for_direct_compatibility(context: &EvalContext) -> EvaluatorStepContext<'_> {
        EvaluatorStepContext {
            admission: EvaluatorStepAdmission::DirectCompatibility,
            context,
            pending_reflection_activations: RefCell::new(Vec::new()),
            _thread_bound: PhantomData,
        }
    }

    pub(crate) fn context(&self) -> &EvalContext {
        self.context
    }

    pub(crate) fn with_value_access<R>(
        &self,
        operation: impl for<'scope> FnOnce(EvaluationValueAccess<'scope>) -> R,
    ) -> R {
        match self.admission {
            EvaluatorStepAdmission::Poll(poll) => poll.with_value_access(self.context, operation),
            EvaluatorStepAdmission::DirectCompatibility => {
                self.context.values().with_runtime_value_access(|values| {
                    let access = EvaluationValueAccess::try_new(self.context, values)
                        .expect("direct evaluator access must retain its value domain");
                    operation(access)
                })
            }
        }
    }

    /// Compatibility root publication for a currently bare evaluator result.
    /// I4F.2 replaces the wrapper with a collector root without changing this
    /// step-owned boundary.
    pub(crate) fn root_value(&self, value: Value) -> RuntimeValueRoot {
        RuntimeValueRoot::new(self.context.values(), value)
    }

    /// Compatibility failure publication for one bounded evaluator result.
    pub(crate) fn root_failure(&self, failure: Arc<EvaluationFailure>) -> RuntimeFailureRoot {
        RuntimeFailureRoot::new(self.context.values(), failure)
    }

    /// Projects one owned completion back into the active evaluator step.
    ///
    /// Wait and task observers outside evaluation retain the root. The bare
    /// semantic value exists only inside this explicitly bounded managed
    /// access region.
    pub(crate) fn project_root(&self, root: &RuntimeValueRoot) -> Value {
        assert_eq!(
            root.runtime_id(),
            self.context.values().runtime_id(),
            "wait completion and evaluator context must share one value domain"
        );
        self.with_value_access(|access| access.clone_root(root))
    }

    pub(crate) fn defer_reflection_activation(&self, task: ReflectionTaskReservation) {
        self.pending_reflection_activations.borrow_mut().push(task);
    }

    pub(crate) fn finish(mut self) {
        let pending = std::mem::take(self.pending_reflection_activations.get_mut());
        drop(self);
        for task in pending {
            task.activate();
        }
    }
}

/// Ephemeral poll authority for opening bounded evaluator regions.
///
/// This context contains no mutator or managed borrow and may remain on the
/// orchestration stack while a callback, wait, or publication occurs. Only
/// `with_value_access` activates the matching heap, and that activation ends
/// before the method returns. Its temporary strong demand route comes from
/// either a detached, runtime-checked claim or the explicit owner of a direct
/// effect/search poll and is not exposed to the machine.
pub(crate) struct EvaluationPollContext {
    demand: Arc<EvaluationDemandState>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl EvaluationPollContext {
    pub(in crate::evaluation) fn for_claim(claim: &ClaimedDemandSession) -> Self {
        let demand = claim.demand();
        #[cfg(test)]
        demand.poll_contexts.fetch_add(1, Ordering::Relaxed);
        Self {
            demand,
            _thread_bound: PhantomData,
        }
    }

    /// Opens the same ephemeral orchestration carrier for a caller-driven
    /// poll which has no detached coordinator claim.
    ///
    /// Direct effect runs and isolated searches own an explicit demand
    /// session instead. This constructor does not activate managed access;
    /// it only retains that checked session for the duration of one poll.
    pub(crate) fn for_context(context: &EvalContext) -> Self {
        #[cfg(test)]
        context
            .session
            .poll_contexts
            .fetch_add(1, Ordering::Relaxed);
        Self {
            demand: context.session.clone(),
            _thread_bound: PhantomData,
        }
    }

    pub(crate) fn assert_context(&self, context: &EvalContext) {
        assert!(
            Arc::ptr_eq(&self.demand, &context.session),
            "poll context and evaluator context must share one demand session"
        );
    }

    pub(crate) fn with_value_access<R>(
        &self,
        context: &EvalContext,
        operation: impl for<'scope> FnOnce(EvaluationValueAccess<'scope>) -> R,
    ) -> R {
        self.assert_context(context);
        self.demand.values.with_runtime_value_access(|values| {
            let access = EvaluationValueAccess::try_new(context, values)
                .expect("poll context and managed access must share one value domain");
            operation(access)
        })
    }

    /// Derives the thread-bound evaluator authority for one machine substep.
    pub(crate) fn evaluator<'step>(
        &'step self,
        context: &'step EvalContext,
    ) -> EvaluatorStepContext<'step> {
        self.assert_context(context);
        EvaluatorStepContext {
            admission: EvaluatorStepAdmission::Poll(self),
            context,
            pending_reflection_activations: RefCell::new(Vec::new()),
            _thread_bound: PhantomData,
        }
    }

    /// Runs one callback-free evaluator phase, then activates every external
    /// reflection task it discovered only after its evaluator carrier ends.
    pub(crate) fn evaluate<R>(
        &self,
        context: &EvalContext,
        operation: impl FnOnce(&EvaluatorStepContext<'_>) -> R,
    ) -> R {
        let evaluator = self.evaluator(context);
        let result = operation(&evaluator);
        evaluator.finish();
        result
    }

    /// Test-fixture convenience for publishing a compatibility result without
    /// pretending the poll context carries managed access.
    #[cfg(test)]
    pub(crate) fn root_value(&self, value: Value) -> RuntimeValueRoot {
        RuntimeValueRoot::new(&self.demand.values, value)
    }

    /// Roots a machine failure before it crosses the poll boundary.
    pub(crate) fn root_failure(&self, failure: Arc<EvaluationFailure>) -> RuntimeFailureRoot {
        RuntimeFailureRoot::new(&self.demand.values, failure)
    }
}

#[cfg(test)]
mod tests {
    use glam_gc::CollectionError;

    use crate::core::CoreValueFactory;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

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

    assert_does_not_implement!(
        runtime_value_access_is_not_send,
        RuntimeValueAccess<'static>,
        Send
    );
    assert_does_not_implement!(
        runtime_value_access_is_not_sync,
        RuntimeValueAccess<'static>,
        Sync
    );
    assert_does_not_implement!(
        evaluation_value_access_is_not_send,
        EvaluationValueAccess<'static>,
        Send
    );
    assert_does_not_implement!(
        evaluation_value_access_is_not_sync,
        EvaluationValueAccess<'static>,
        Sync
    );
    assert_does_not_implement!(
        evaluator_step_context_is_not_send,
        EvaluatorStepContext<'static>,
        Send
    );
    assert_does_not_implement!(
        evaluator_step_context_is_not_sync,
        EvaluatorStepContext<'static>,
        Sync
    );

    fn value_factory() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    fn invoke_callback<R>(callback: impl FnOnce() -> R) -> R {
        callback()
    }

    #[test]
    fn scoped_authority_is_thread_bound_while_durable_context_is_send() {
        fn assert_send<T: Send>() {}

        assert_send::<EvalContext>();
    }

    #[test]
    fn poll_context_opens_two_scopes_around_a_mutator_free_callback() {
        let values = value_factory();
        let context = EvalContext::isolated(values.clone());
        let poll = EvaluationPollContext::for_context(&context);
        let evaluator = poll.evaluator(&context);

        let first = evaluator.with_value_access(|access| {
            assert!(access.values.belongs_to(context.values()));
            assert!(matches!(
                values.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));
            17
        });
        assert_eq!(first, 17);

        let mut callback_ran = false;
        let callback_result = invoke_callback(|| {
            callback_ran = true;
            let collector_values = values.clone();
            std::thread::spawn(move || collector_values.collect_managed_for_test())
                .join()
                .expect("the cross-thread collection probe must not panic")
                .expect("the callback must run without an active mutator")
        });
        assert!(callback_ran);
        assert_eq!(callback_result.root_entries(), 0);

        let second = evaluator.with_value_access(|access| {
            assert!(matches!(
                values.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));
            let allocator = access
                .values
                .allocator::<u64>()
                .expect("the scoped u64 allocation should fit");
            access.values.root(allocator.alloc(29))
        });

        evaluator.with_value_access(|access| {
            assert_eq!(*access.values.get(&second), 29);
        });
    }

    #[test]
    fn evaluation_scope_reuses_recursive_same_heap_entry() {
        let values = value_factory();
        let context = EvalContext::isolated(values.clone());
        let poll = EvaluationPollContext::for_context(&context);

        poll.with_value_access(&context, |outer| {
            assert!(matches!(
                values.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));
            poll.with_value_access(&context, |inner| {
                assert!(outer.values.belongs_to(context.values()));
                assert!(inner.values.belongs_to(context.values()));
                assert!(matches!(
                    values.collect_managed_for_test(),
                    Err(CollectionError::ActiveMutator)
                ));
            });
            assert!(matches!(
                values.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));
        });

        values
            .collect_managed_for_test()
            .expect("recursive admission must release the outermost mutator once");
    }

    #[test]
    fn different_heap_authority_is_rejected() {
        let owner = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let other_runtime =
            CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let context = EvalContext::isolated(owner);

        other_runtime.with_runtime_value_access(|values| {
            assert!(matches!(
                EvaluationValueAccess::try_new(&context, values),
                Err(ValueAccessDomainMismatch)
            ));
        });
    }

    #[test]
    fn runtime_tls_caches_remain_heap_qualified() {
        let _ = glam_gc::Heap::release_current_thread_caches();
        let first = value_factory();
        let second = value_factory();

        first.with_runtime_value_access(|first_access| {
            assert!(first_access.belongs_to(&first));
            assert!(!first_access.belongs_to(&second));
            second.with_runtime_value_access(|second_access| {
                assert!(second_access.belongs_to(&second));
                assert!(!second_access.belongs_to(&first));
                assert!(first_access.belongs_to(&first));
            });
        });

        assert_eq!(
            glam_gc::Heap::release_current_thread_caches(),
            2,
            "nested runtime access should create one independent TLS cache per heap"
        );
    }

    #[test]
    fn poll_context_without_scope_carries_no_heap_authority() {
        let values = value_factory();
        let context = EvalContext::isolated(values.clone());
        let poll = EvaluationPollContext::for_context(&context);

        values
            .collect_managed_for_test()
            .expect("constructing a poll context must not enter the managed heap");
        poll.with_value_access(&context, |access| {
            assert!(access.values.belongs_to(&values));
            assert!(matches!(
                values.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));
        });
        values
            .collect_managed_for_test()
            .expect("managed authority must end with the bounded access callback");
    }
}
