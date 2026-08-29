//! Scoped managed-value authority for evaluator substeps.
//!
//! A scheduler-created poll context carries no active mutator. It may open a
//! bounded callback-free evaluator region, whose lifetime-bound authority
//! cannot enter durable machine state or cross a thread. Production machine,
//! client-demand, and spark polls receive this context; I3B-I3D partition the
//! opaque evaluator operations which may safely open it.

use crate::core::RuntimeValueAccess;
use crate::core::Value;
use crate::runtime::RuntimeValueRoot;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use super::coordinator::ClaimedDemandSession;
use super::{EvalContext, EvaluationDemandState};

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
/// the scheduler-owned admission route established by I3A.
pub(crate) struct EvaluatorStepContext<'step> {
    admission: EvaluatorStepAdmission<'step>,
    context: &'step EvalContext,
    _thread_bound: PhantomData<Rc<()>>,
}

enum EvaluatorStepAdmission<'step> {
    Claimed(&'step EvaluationPollContext),
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

    pub(crate) fn values(&self) -> &RuntimeValueAccess<'scope> {
        &self.values
    }
}

impl EvaluatorStepContext<'_> {
    pub(crate) fn for_direct_compatibility(context: &EvalContext) -> EvaluatorStepContext<'_> {
        EvaluatorStepContext {
            admission: EvaluatorStepAdmission::DirectCompatibility,
            context,
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
            EvaluatorStepAdmission::Claimed(poll) => {
                poll.with_value_access(self.context, operation)
            }
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
}

/// Ephemeral scheduler authority for opening bounded evaluator regions.
///
/// This context contains no mutator or managed borrow and may remain on the
/// orchestration stack while a callback, wait, or publication occurs. Only
/// `with_value_access` activates the matching heap, and that activation ends
/// before the method returns. Its temporary strong demand route is cloned from
/// a detached, runtime-checked claim and is not exposed to the machine.
pub(crate) struct EvaluationPollContext {
    demand: Arc<EvaluationDemandState>,
}

impl EvaluationPollContext {
    pub(in crate::evaluation) fn for_claim(claim: &ClaimedDemandSession) -> Self {
        Self {
            demand: claim.demand(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_context(context: &EvalContext) -> Self {
        Self {
            demand: context.session.clone(),
        }
    }

    pub(crate) fn with_value_access<R>(
        &self,
        context: &EvalContext,
        operation: impl for<'scope> FnOnce(EvaluationValueAccess<'scope>) -> R,
    ) -> R {
        assert!(
            Arc::ptr_eq(&self.demand, &context.session),
            "poll context and evaluator context must share one demand session"
        );
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
        assert!(
            Arc::ptr_eq(&self.demand, &context.session),
            "poll context and evaluator context must share one demand session"
        );
        EvaluatorStepContext {
            admission: EvaluatorStepAdmission::Claimed(self),
            context,
            _thread_bound: PhantomData,
        }
    }

    /// Wrap one currently bare evaluator result before it leaves the machine
    /// poll boundary.
    ///
    /// This is the I3A.4 compatibility seam. `RuntimeValueRoot` still stores a
    /// bare `Value`, so no managed pointer is being rooted here yet. I3B moves
    /// this operation into the bounded evaluator scope before I4F.2 changes
    /// the root's representation.
    pub(crate) fn root_value(&self, value: Value) -> RuntimeValueRoot {
        RuntimeValueRoot::new(&self.demand.values, value)
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
            assert!(access.values().belongs_to(context.values()));
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
                .values()
                .allocator::<u64>()
                .expect("the scoped u64 allocation should fit");
            access.values().root(allocator.alloc(29))
        });

        evaluator.with_value_access(|access| {
            assert_eq!(*access.values().get(&second), 29);
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
                assert!(outer.values().belongs_to(context.values()));
                assert!(inner.values().belongs_to(context.values()));
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
        let runtime = allocate_evaluation_runtime_id();
        let owner = CoreValueFactory::new(runtime, RuntimeIds::new());
        let other_heap_with_same_runtime_id = CoreValueFactory::new(runtime, RuntimeIds::new());
        let context = EvalContext::isolated(owner);

        other_heap_with_same_runtime_id.with_runtime_value_access(|values| {
            assert!(matches!(
                EvaluationValueAccess::try_new(&context, values),
                Err(ValueAccessDomainMismatch)
            ));
        });
    }
}
