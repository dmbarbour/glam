//! Scoped managed-value authority for evaluator substeps.
//!
//! A poll context lends its quantum's mutator to bounded callback-free
//! evaluator regions, whose lifetime-bound authority cannot enter durable
//! machine state or cross a thread. Machine, client-demand, spark,
//! direct-effect, and isolated-search polls receive this context.

#[cfg(test)]
use crate::core::ManagedCoreNetRoot;
use crate::core::{
    EvaluationFailure, LazyValue, ManagedLazyAccess, ManagedLazyRoot, ManagedPromiseAccess,
    ManagedPromiseRoot, PromisedValue, RuntimeValueAccess, RuntimeValueMutator, Value,
};
#[cfg(test)]
use crate::core_net::CoreSpecialization;
use crate::core_net::{CoreRuntimeNet, CoreRuntimeNetAccess};
#[cfg(test)]
use crate::interaction_net::RuntimeNet;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::Ordering;

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
/// to open smaller managed-access regions. Private construction keeps poll
/// admission its only entry route.
pub(crate) struct EvaluatorStepContext<'step> {
    poll: &'step PollShared,
    /// Access borrowed from the poll's mutator.
    access: RuntimeValueAccess<'step>,
    context: &'step EvalContext,
    pending_reflection_activations: RefCell<Vec<ReflectionTaskReservation>>,
    pending_managed_publications: RefCell<Vec<PendingManagedPublication>>,
    _thread_bound: PhantomData<Rc<()>>,
}

/// Temporary liveness carried only until one evaluator step has installed its
/// fresh managed results into their real cache, net, or runtime root.
///
/// This is deliberately family-specific rather than a general value owner:
/// existing semantic values do not acquire another root merely by passing
/// through evaluation.
#[expect(
    dead_code,
    reason = "variant payloads are retained solely as exact temporary owners until evaluator-step publication"
)]
enum PendingManagedPublication {
    Lazy(ManagedLazyRoot),
    Promise(ManagedPromiseRoot),
    #[cfg(test)]
    CoreNet(ManagedCoreNetRoot),
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

    /// Derives bounded access to one net owned by this value domain.
    pub(crate) fn net<'access>(
        &'access self,
        runtime: &'access CoreRuntimeNet,
    ) -> CoreRuntimeNetAccess<'access, 'scope> {
        runtime.access(&self.values)
    }

    pub(crate) fn lazy<'access>(
        &'access self,
        lazy: &'access LazyValue,
    ) -> ManagedLazyAccess<'access, 'scope> {
        lazy.access(&self.values)
    }

    #[inline]
    pub(crate) fn lazy_root<'access>(
        &'access self,
        lazy: &'access ManagedLazyRoot,
    ) -> ManagedLazyAccess<'access, 'scope> {
        lazy.access(&self.values)
            .expect("evaluator access and rooted lazy must share one value domain")
    }

    pub(crate) fn promise<'access>(
        &'access self,
        promise: &'access PromisedValue,
    ) -> ManagedPromiseAccess<'access, 'scope> {
        promise.access(&self.values)
    }

    #[cfg(test)]
    pub(crate) fn promise_root<'access>(
        &'access self,
        promise: &'access ManagedPromiseRoot,
    ) -> ManagedPromiseAccess<'access, 'scope> {
        promise
            .access(&self.values)
            .expect("evaluator access and rooted promise must share one value domain")
    }

    /// Clones one compatibility root only while matching value-domain access
    /// is admitted for this evaluator step.
    pub(crate) fn clone_root(&self, root: &RuntimeValueRoot) -> Value {
        root.clone_core_with(&self.values)
    }

    pub(crate) fn root_value(&self, value: Value) -> RuntimeValueRoot {
        self.values.root_runtime_value(value)
    }
}

#[cfg(test)]
impl crate::core::TestAccess for EvaluatorStepContext<'_> {
    fn test_access<R>(
        &self,
        operation: impl for<'scope> FnOnce(&RuntimeValueAccess<'scope>) -> R,
    ) -> R {
        self.with_value_access(|access| operation(access.values()))
    }
}

#[cfg(test)]
impl crate::core::TestAccess for EvaluationValueAccess<'_> {
    fn test_access<R>(
        &self,
        operation: impl for<'scope> FnOnce(&RuntimeValueAccess<'scope>) -> R,
    ) -> R {
        operation(self.values())
    }
}

impl EvaluatorStepContext<'_> {
    pub(crate) fn context(&self) -> &EvalContext {
        self.context
    }

    /// Reads `promise` as an evaluator step would: its assignment, or a halt
    /// blocked on it while it is unassigned. Test sources use this to depend
    /// on an input without nesting a synchronous driver inside their poll,
    /// which may not wait under the poll's mutator.
    #[cfg(test)]
    pub(crate) fn promise_or_block_for_test(
        &self,
        promise: &PromisedValue,
    ) -> Result<Value, crate::core::EvaluationHalt> {
        self.with_value_access(|access| match access.promise(promise).assignment() {
            Some(Ok(value)) => Ok(value),
            Some(Err(failure)) => Err(crate::core::EvaluationHalt::failure(failure)),
            None => Err(crate::core::EvaluationHalt::unassigned_root(
                promise.root_in(access.values()),
            )),
        })
    }

    /// Runs `work` once its poll's heap admission ends; see
    /// [`EvaluationPollContext::after_admission`].
    pub(crate) fn after_admission(&self, work: impl FnOnce() + 'static) {
        self.poll.after_admission(work);
    }

    #[inline]
    pub(crate) fn with_value_access<R>(
        &self,
        operation: impl for<'scope> FnOnce(EvaluationValueAccess<'scope>) -> R,
    ) -> R {
        self.access.with_reborrow(|values| {
            operation(
                EvaluationValueAccess::try_new(self.context, values)
                    .expect("poll context and managed access must share one value domain"),
            )
        })
    }

    /// Roots one bare evaluator result in a collector-backed runtime root at
    /// this step-owned boundary.
    pub(crate) fn root_value(
        &self,
        construct: impl for<'scope> FnOnce(&EvaluationValueAccess<'scope>) -> Value,
    ) -> RuntimeValueRoot {
        self.with_value_access(|access| {
            let value = construct(&access);
            access.root_value(value)
        })
    }

    /// Compatibility failure publication for one bounded evaluator result.
    pub(crate) fn root_failure(&self, failure: Arc<EvaluationFailure>) -> RuntimeFailureRoot {
        self.with_value_access(|access| access.values().root_runtime_failure(failure))
    }

    /// Constructs a lazy with a temporary exact owner which survives every
    /// access boundary in this evaluator step. `finish` drops that owner only
    /// after the step has published its result or durable blocked state.
    #[cfg(test)]
    pub(crate) fn construct_lazy(
        &self,
        construct: impl for<'scope> FnOnce(&RuntimeValueAccess<'scope>) -> LazyValue,
    ) -> LazyValue {
        let (value, root) = self.with_value_access(|access| {
            let value = construct(access.values());
            let root = value.root_in(access.values());
            let value = LazyValue::from_root(&root, access.values());
            (value, root)
        });
        self.pending_managed_publications
            .borrow_mut()
            .push(PendingManagedPublication::Lazy(root));
        value
    }

    #[cfg(test)]
    pub(crate) fn construct_promise(&self) -> PromisedValue {
        let (value, root) = self.with_value_access(|access| {
            let root = access
                .values()
                .construct_rooted_managed_promise()
                .expect("managed promise representation must fit one collector run");
            let value = PromisedValue::from_root(&root, access.values());
            (value, root)
        });
        self.pending_managed_publications
            .borrow_mut()
            .push(PendingManagedPublication::Promise(root));
        value
    }

    #[cfg(test)]
    pub(crate) fn construct_core_net(
        &self,
        runtime: RuntimeNet<CoreSpecialization>,
    ) -> CoreRuntimeNet {
        let (value, root) = self.with_value_access(|access| {
            let root = access
                .values()
                .construct_rooted_managed_core_net(runtime)
                .expect("managed core-net representation must fit one collector run");
            let value = CoreRuntimeNet::from_root(&root, access.values());
            (value, root)
        });
        self.pending_managed_publications
            .borrow_mut()
            .push(PendingManagedPublication::CoreNet(root));
        value
    }

    /// Projects one owned completion back into the active evaluator step.
    ///
    /// Wait and task observers outside evaluation retain the root. The bare
    /// semantic value exists only inside this explicitly bounded managed
    /// access region.
    pub(crate) fn project_root<R>(
        &self,
        root: &RuntimeValueRoot,
        operation: impl for<'scope> FnOnce(&EvaluationValueAccess<'scope>, Value) -> R,
    ) -> R {
        assert_eq!(
            root.runtime_id(),
            self.context.values().runtime_id(),
            "wait completion and evaluator context must share one value domain"
        );
        self.with_value_access(|access| {
            let value = access.clone_root(root);
            operation(&access, value)
        })
    }

    pub(crate) fn defer_reflection_activation(&self, task: ReflectionTaskReservation) {
        self.pending_reflection_activations.borrow_mut().push(task);
    }

    pub(crate) fn finish(mut self) {
        let pending = std::mem::take(self.pending_reflection_activations.get_mut());
        let managed = std::mem::take(self.pending_managed_publications.get_mut());
        let poll = self.poll;
        drop(self);
        drop(managed);
        // A launcher may be client code, so it runs outside heap admission.
        for task in pending {
            poll.after_admission(move || task.activate());
        }
    }
}

/// Work a quantum runs once its heap admission ends, in order.
pub(in crate::evaluation) type DeferredWork = Vec<Box<dyn FnOnce()>>;

/// Runs the work a quantum deferred, once its mutator is suspended or ended.
pub(in crate::evaluation) fn run_deferred(deferred: DeferredWork) {
    for work in deferred {
        work();
    }
}

/// The part of a poll's authority that outlives its mutator: its demand
/// route, and the work it defers until its heap admission ends.
pub(crate) struct PollShared {
    demand: Arc<EvaluationDemandState>,
    /// Work deferred until the poll's mutator ends.
    after_admission: RefCell<DeferredWork>,
}

impl PollShared {
    fn new(demand: Arc<EvaluationDemandState>) -> Self {
        #[cfg(test)]
        demand.poll_contexts.fetch_add(1, Ordering::Relaxed);
        Self {
            demand,
            after_admission: RefCell::new(Vec::new()),
        }
    }

    fn after_admission(&self, work: impl FnOnce() + 'static) {
        self.after_admission.borrow_mut().push(Box::new(work));
    }

    fn assert_context(&self, context: &EvalContext) {
        assert!(
            Arc::ptr_eq(&self.demand, &context.session),
            "poll context and evaluator context must share one demand session"
        );
    }
}

/// Ephemeral poll authority for evaluator work.
///
/// A quantum's context carries the thread's mutator for its runtime
/// ([`Self::quantum`]): value access borrows it rather than entering the heap,
/// and work that must not run under it is deferred until it ends. Tests poll
/// in quanta too. The context's temporary strong demand route comes from
/// either a detached, runtime-checked claim or the explicit owner of a direct
/// effect/search poll and is not exposed to the machine.
pub(crate) struct EvaluationPollContext<'lend, 'mu> {
    shared: PollShared,
    mutator: RuntimeValueMutator<'lend, 'mu>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl<'lend, 'mu> EvaluationPollContext<'lend, 'mu> {
    /// This quantum's mutator, when it is the one for `runtime`.
    pub(in crate::evaluation) fn mutator_for(
        &mut self,
        runtime: crate::runtime::EvaluationRuntimeId,
    ) -> Option<&mut RuntimeValueMutator<'lend, 'mu>> {
        Some(&mut self.mutator).filter(|mutator| mutator.runtime_id() == runtime)
    }
}

impl EvaluationPollContext<'_, '_> {
    /// Runs one quantum of `demand`'s work holding the thread's mutator,
    /// which `operation`'s context lends to its value accesses. The
    /// scheduler's quantum boundaries call this around one claimed poll; the
    /// mutator ends before the boundary services collection pressure.
    ///
    /// Work the poll defers with [`Self::after_admission`] runs once the
    /// mutator has ended, in order, before this returns and so before the
    /// driver releases its claim. If the poll unwinds, deferred work is
    /// dropped.
    pub(crate) fn quantum<R>(
        demand: Arc<EvaluationDemandState>,
        operation: impl FnOnce(&mut EvaluationPollContext<'_, '_>) -> R,
    ) -> R {
        let values = demand.values.clone();
        let (result, deferred) = values.with_runtime_value_mutator(|mut mutator| {
            Self::quantum_under(&mut mutator, demand, operation)
        });
        run_deferred(deferred);
        result
    }

    /// Runs one quantum of `demand`'s work under a driver's `mutator`, which
    /// the quantum's context borrows (`driver-mutator`). Returns the result
    /// with the work the poll deferred, which the driver runs at the quantum
    /// boundary with the mutator suspended ([`run_deferred`]).
    pub(in crate::evaluation) fn quantum_under<R>(
        mutator: &mut RuntimeValueMutator<'_, '_>,
        demand: Arc<EvaluationDemandState>,
        operation: impl FnOnce(&mut EvaluationPollContext<'_, '_>) -> R,
    ) -> (R, DeferredWork) {
        debug_assert_eq!(mutator.runtime_id(), demand.values.runtime_id());
        let mut context = EvaluationPollContext {
            shared: PollShared::new(demand),
            mutator: mutator.reborrow(),
            _thread_bound: PhantomData,
        };
        let result = operation(&mut context);
        let deferred = context.shared.after_admission.take();
        (result, deferred)
    }

    /// Runs one quantum, as [`Self::quantum_for_context`] does, under a
    /// driver's `mutator`. Work the poll deferred runs under one suspension
    /// once the poll ends.
    pub(crate) fn quantum_for_context_under<R>(
        mutator: &mut RuntimeValueMutator<'_, '_>,
        context: &EvalContext,
        operation: impl FnOnce(&mut EvaluationPollContext<'_, '_>) -> R,
    ) -> R {
        let (result, deferred) = Self::quantum_under(mutator, context.session.clone(), operation);
        if !deferred.is_empty() {
            mutator.suspend(|| run_deferred(deferred));
        }
        result
    }

    /// Runs one quantum, as [`Self::quantum`] does, for a caller-driven poll
    /// which has no detached coordinator claim.
    pub(crate) fn quantum_for_context<R>(
        context: &EvalContext,
        operation: impl FnOnce(&mut EvaluationPollContext<'_, '_>) -> R,
    ) -> R {
        Self::quantum(context.session.clone(), operation)
    }

    /// Runs `work` once this poll's mutator ends. Work that waits on another
    /// thread or calls client code belongs here, so a collection never waits
    /// for it. Machines reach it through their evaluator step
    /// ([`EvaluatorStepContext::after_admission`]).
    #[cfg(test)]
    pub(crate) fn after_admission(&self, work: impl FnOnce() + 'static) {
        self.shared.after_admission(work);
    }

    /// Releases this quantum's heap admission while `operation` runs, then
    /// re-admits. Client callbacks and work
    /// that waits on another thread run here when their result is needed at
    /// once. A suspension costs about two heap entries, so adjacent
    /// operations share one.
    pub(crate) fn suspend<R>(&mut self, operation: impl FnOnce() -> R) -> R {
        self.mutator.suspend(operation)
    }

    /// Whether this quantum has deferred work, so it should end rather than
    /// poll on.
    pub(crate) fn has_deferred_work(&self) -> bool {
        !self.shared.after_admission.borrow().is_empty()
    }

    pub(crate) fn assert_context(&self, context: &EvalContext) {
        self.shared.assert_context(context);
    }

    #[inline]
    pub(crate) fn with_value_access<R>(
        &self,
        context: &EvalContext,
        operation: impl for<'scope> FnOnce(EvaluationValueAccess<'scope>) -> R,
    ) -> R {
        self.shared.assert_context(context);
        self.mutator.with_access(|values| {
            operation(
                EvaluationValueAccess::try_new(context, values)
                    .expect("poll context and managed access must share one value domain"),
            )
        })
    }

    /// Derives the thread-bound evaluator authority for one machine substep.
    #[inline]
    pub(crate) fn evaluator<'step>(
        &'step self,
        context: &'step EvalContext,
    ) -> EvaluatorStepContext<'step> {
        self.shared.assert_context(context);
        EvaluatorStepContext {
            poll: &self.shared,
            access: self.mutator.access(),
            context,
            pending_reflection_activations: RefCell::new(Vec::new()),
            pending_managed_publications: RefCell::new(Vec::new()),
            _thread_bound: PhantomData,
        }
    }

    /// Runs one callback-free evaluator phase, then activates every external
    /// reflection task it discovered only after its evaluator carrier ends.
    #[inline]
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

    /// Re-admits this quantum's mutator after a suspension unwound into a
    /// panic the caller caught; see `glam_gc::Mutator::readmit`.
    pub(crate) fn readmit(&mut self) {
        self.mutator.readmit();
    }

    /// Borrows this quantum's runtime access for `operation`.
    pub(crate) fn with_runtime_access<R>(
        &self,
        operation: impl for<'scope> FnOnce(RuntimeValueAccess<'scope>) -> R,
    ) -> R {
        self.mutator.with_access(operation)
    }

    /// Test-fixture convenience for publishing a compatibility result.
    #[cfg(test)]
    pub(crate) fn root_value(&self, value: Value) -> RuntimeValueRoot {
        self.with_runtime_access(|access| access.root_runtime_value(value))
    }

    #[cfg(test)]
    pub(crate) fn root_unit(&self) -> RuntimeValueRoot {
        self.with_runtime_access(|access| access.root_runtime_value(access.unit()))
    }

    #[cfg(test)]
    pub(crate) fn duplicate_and_root_value(&self, value: &Value) -> RuntimeValueRoot {
        self.with_runtime_access(|access| access.root_runtime_value(access.duplicate_value(value)))
    }

    /// Roots a machine failure before it crosses the poll boundary.
    pub(crate) fn root_failure(&self, failure: Arc<EvaluationFailure>) -> RuntimeFailureRoot {
        self.with_runtime_access(|access| access.root_runtime_failure(failure))
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
    fn a_quantum_suspends_its_mutator_around_a_callback() {
        let values = value_factory();
        let baseline = values
            .collect_managed_for_test()
            .expect("canonical roots should collect before the access fixture");
        let context = EvalContext::isolated(values.clone());
        EvaluationPollContext::quantum_for_context(&context, |poll| {
            let first = poll.with_value_access(&context, |access| {
                assert!(access.values.belongs_to(context.values()));
                assert!(matches!(
                    values.collect_managed_for_test(),
                    Err(CollectionError::ActiveMutator)
                ));
                17
            });
            assert_eq!(first, 17);

            let mut callback_ran = false;
            let callback_result = poll.suspend(|| {
                invoke_callback(|| {
                    callback_ran = true;
                    let collector_values = values.clone();
                    std::thread::spawn(move || collector_values.collect_managed_for_test())
                        .join()
                        .expect("the cross-thread collection probe must not panic")
                        .expect("the callback must run without an active mutator")
                })
            });
            assert!(callback_ran);
            assert_eq!(callback_result.root_entries(), baseline.root_entries());

            let second = poll.with_value_access(&context, |access| {
                assert!(matches!(
                    values.collect_managed_for_test(),
                    Err(CollectionError::ActiveMutator)
                ));
                let allocator = access
                    .values
                    .allocator::<u64>()
                    .expect("the scoped u64 allocation should fit");
                access.values.root(allocator.alloc_view(29))
            });

            poll.with_value_access(&context, |access| {
                assert_eq!(*access.values.get(&second), 29);
            });
        });
    }

    #[test]
    fn evaluator_step_results_survive_until_their_containing_root_publishes() {
        let values = value_factory();
        values
            .collect_managed_for_test()
            .expect("the evaluator publication fixture should start collectible");
        let context = EvalContext::isolated(values.clone());

        let root = crate::evaluation::EvalContext::evaluate_test_step(&context, |evaluator| {
            let lazy = evaluator
                .construct_lazy(|access| LazyValue::error_in(access, "guarded evaluator lazy"));
            let promise = evaluator.construct_promise();
            let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
            let exposed = builder.data(Value::Number(31.into()));
            let template = builder.finish(exposed);
            let runtime =
                evaluator.with_value_access(|access| template.instantiate_with(access.values()));
            let net = evaluator.construct_core_net(runtime);

            // The step holds its quantum's mutator, so no collection runs
            // before its fresh results reach their containing root.
            assert!(matches!(
                values.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));

            evaluator.root_value(|_| {
                Value::List(crate::core::List::from_values(vec![
                    Value::Lazy(lazy),
                    Value::Promised(promise),
                    Value::Net(crate::core::NetValue::new(net)),
                ]))
            })
        });

        let retained = values
            .collect_managed_for_test()
            .expect("the containing runtime root should retain all three managed families");
        assert!(retained.marked_slots() >= 4);
        drop(root);
        let retired = values
            .collect_managed_for_test()
            .expect("dropping the containing root should retire the guarded graph");
        assert!(retired.finalized_slots() >= 4);
    }

    #[test]
    fn nested_evaluator_accesses_borrow_one_quantum_mutator() {
        let values = value_factory();
        let context = EvalContext::isolated(values.clone());

        EvaluationPollContext::quantum_for_context(&context, |poll| {
            poll.with_value_access(&context, |outer| {
                assert!(matches!(
                    values.collect_managed_for_test(),
                    Err(CollectionError::ActiveMutator)
                ));
                poll.with_value_access(&context, |inner| {
                    assert!(outer.values.belongs_to(context.values()));
                    assert!(inner.values.belongs_to(context.values()));
                });
            });
        });

        values
            .collect_managed_for_test()
            .expect("the quantum's mutator ends with the quantum");
    }

    #[test]
    fn different_heap_authority_is_rejected() {
        let owner = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let other_runtime =
            CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let context = EvalContext::isolated(owner);

        other_runtime.with_boundary_access(|values| {
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
        let first_scoped = first.scoped();
        let second = value_factory();

        first_scoped.with_boundary_access(|first_access| {
            assert!(std::ptr::eq(first_access.values(), &first_scoped));
            assert!(first_access.belongs_to(&first));
            assert!(!first_access.belongs_to(&second));
            // A thread holds one runtime's value access at a time.
            let nested = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                second.with_boundary_access(|_| {});
            }));
            assert!(nested.is_err());
            assert!(first_access.belongs_to(&first));
        });
        second.with_boundary_access(|second_access| {
            assert!(second_access.belongs_to(&second));
            assert!(!second_access.belongs_to(&first));
        });

        assert_eq!(
            glam_gc::Heap::release_current_thread_caches(),
            2,
            "access to two runtimes in turn should create one independent TLS cache per heap"
        );
    }

    #[test]
    fn poll_context_without_scope_carries_no_heap_authority() {
        let values = value_factory();
        let context = EvalContext::isolated(values.clone());

        values
            .collect_managed_for_test()
            .expect("constructing a poll context must not enter the managed heap");
        crate::evaluation::EvalContext::with_test_value_access(&context, |access| {
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

    #[test]
    fn a_quantum_runs_deferred_work_after_its_mutator_in_order() {
        let values = value_factory();
        let context = EvalContext::isolated(values.clone());
        let log = Rc::new(RefCell::new(Vec::new()));

        let result = EvaluationPollContext::quantum(context.session.clone(), |quantum| {
            assert!(matches!(
                values.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));
            // Value access borrows the quantum's mutator.
            quantum.with_value_access(&context, |access| {
                assert!(access.values().belongs_to(&values));
            });
            for name in ["first", "second"] {
                let (log, values) = (log.clone(), values.clone());
                quantum.after_admission(move || {
                    values
                        .collect_managed_for_test()
                        .expect("deferred work runs once the mutator has ended");
                    log.borrow_mut().push(name);
                });
            }
            assert!(quantum.has_deferred_work());
            log.borrow_mut().push("poll");
            7
        });
        assert_eq!(result, 7);
        assert_eq!(*log.borrow(), ["poll", "first", "second"]);
    }

    #[test]
    fn an_unwinding_quantum_drops_its_deferred_work() {
        let values = value_factory();
        let context = EvalContext::isolated(values.clone());
        let ran = Rc::new(RefCell::new(false));

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            EvaluationPollContext::quantum(context.session.clone(), |quantum| {
                let ran = ran.clone();
                quantum.after_admission(move || *ran.borrow_mut() = true);
                panic!("the poll unwinds");
            })
        }));
        assert!(unwound.is_err());
        assert!(!*ran.borrow());
        values
            .collect_managed_for_test()
            .expect("the unwound mutator must have ended");

        // The thread can hold a mutator again afterwards.
        EvaluationPollContext::quantum(context.session.clone(), |_| {});
    }
}
