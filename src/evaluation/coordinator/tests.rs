//! Cross-kind coordinator lifecycle and concurrency tests.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, OnceLock};
use std::thread;

use super::*;

/// Real external ownership beside the machine-facing demand record.
///
/// Coordinator tests intentionally bypass `EvalContext`, but should still
/// make the production ownership boundary explicit instead of treating an
/// `EvaluationSession` owner as if it were demand state.
struct TestDemand {
    owner: Arc<EvaluationSession>,
    demand: Arc<EvaluationDemandState>,
}

impl TestDemand {
    fn new(coordinator: &Arc<EvaluationWorkCoordinator>) -> Self {
        let owner = EvaluationSession::shared(coordinator);
        let demand = owner.demand.clone();
        Self { owner, demand }
    }

    fn context(&self) -> super::super::EvalContext {
        super::super::EvalContext::new(&self.owner)
    }
}

#[test]
fn task_block_dependency_identity_includes_the_runtime() {
    let id = NonZeroU64::new(17).expect("test dependency identity must be nonzero");
    let dependency = WorkDependency::Test(TestWorkDependency {
        runtime: crate::runtime::allocate_evaluation_runtime_id(),
        id,
    });
    let same_dependency = dependency.clone();
    let foreign_dependency = WorkDependency::Test(TestWorkDependency {
        runtime: crate::runtime::allocate_evaluation_runtime_id(),
        id,
    });

    assert_eq!(dependency, same_dependency);
    assert_ne!(dependency, foreign_dependency);
    assert_eq!(
        EvaluationTaskBlock {
            dependency: Some(dependency),
            observed_epoch: None,
            error: None,
        },
        EvaluationTaskBlock {
            dependency: Some(same_dependency),
            observed_epoch: None,
            error: None,
        }
    );
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "published task block dependency must belong to its coordinator runtime")]
fn task_block_publication_asserts_the_runtime_invariant() {
    let runtime = crate::runtime::allocate_evaluation_runtime_id();
    let foreign_runtime = crate::runtime::allocate_evaluation_runtime_id();
    let block = EvaluationTaskBlock {
        dependency: Some(WorkDependency::Test(TestWorkDependency {
            runtime: foreign_runtime,
            id: NonZeroU64::new(23).expect("test dependency identity must be nonzero"),
        })),
        observed_epoch: None,
        error: None,
    };

    debug_assert_task_block_runtime(runtime, &block);
}

#[test]
fn promise_dependency_projects_only_a_task_owned_producer_wait() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let context = session.context().for_effect_task();
    let resolver_owned = PromisedValue::new(context.values(), "resolver-owned promise");
    let task_owned = PromisedValue::fixpoint(&context, "task-owned promise")
        .expect("the local task should own its promise");

    assert!(
        WorkDependency::Promise(resolver_owned.root())
            .producer_wait()
            .is_none()
    );
    assert_eq!(
        WorkDependency::Promise(task_owned.root()).producer_wait(),
        task_owned.task().map(|task| task.wait().clone())
    );
}

struct TestCompletionSource {
    id: NonZeroU64,
    terminal: OnceLock<()>,
    subscriptions: CompletionSubscriptions,
}

impl TestCompletionSource {
    fn new(coordinator: &Arc<EvaluationWorkCoordinator>) -> Arc<Self> {
        let id = coordinator
            .ids
            .evaluation_wait()
            .expect("test completion identity should be available");
        Arc::new(Self {
            id,
            terminal: OnceLock::new(),
            subscriptions: CompletionSubscriptions::for_test(
                coordinator,
                WorkDependencyKey::Test(id.get()),
            ),
        })
    }

    fn runtime_id(&self) -> EvaluationRuntimeId {
        self.subscriptions.runtime
    }

    fn dependency(&self) -> WorkDependency {
        WorkDependency::Test(TestWorkDependency {
            runtime: self.runtime_id(),
            id: self.id,
        })
    }

    fn key(&self) -> WorkDependencyKey {
        self.subscriptions.source
    }

    fn complete(&self) {
        self.subscriptions
            .publish(|| {
                let _ = self.terminal.set(());
                Ok::<_, std::convert::Infallible>(())
            })
            .expect("infallible test completion should publish");
    }

    fn complete_guarded(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &RuntimeMutationGuard<'_>,
    ) -> CompletionWake {
        let ((), wake) = self
            .subscriptions
            .publish_guarded(coordinator, mutation, || {
                let _ = self.terminal.set(());
                Ok::<_, std::convert::Infallible>(())
            })
            .expect("infallible guarded test completion should publish");
        wake
    }

    fn is_terminal(&self) -> bool {
        self.terminal.get().is_some()
    }

    fn subscriber_count(&self) -> usize {
        self.subscriptions.len()
    }

    fn coordinator_is_live(&self) -> bool {
        self.subscriptions
            .coordinator
            .lock()
            .expect("test work-coordinator binding was poisoned")
            .upgrade()
            .is_some()
    }
}

impl EvaluationWorkCoordinator {
    fn park_claimed_test_reflection(
        self: &Arc<Self>,
        mut claimed: ClaimedReflectionWork,
        source: &TestCompletionSource,
        before_insert: impl FnOnce(),
    ) -> WakeRegistration {
        assert_eq!(source.runtime_id(), self.runtime);
        let mutation = self.admission.mutation_guard();
        let (dependency, registration) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&claimed.id)
                .expect("claimed test reflection work must remain registered");
            assert_eq!(record.demand_session, claimed.demand.id());
            assert_eq!(reflection_work(record).task, claimed.task);
            assert!(matches!(record.state, WorkState::Running));
            let reflection = reflection_work_mut(
                state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed test reflection work must remain registered"),
            );
            assert!(reflection.machine.is_none());
            reflection.machine = claimed.machine.take();
            let exact = publish_task_block_locked(
                &mut state,
                self.runtime,
                claimed.id,
                EvaluationTaskBlock {
                    dependency: Some(source.dependency()),
                    observed_epoch: None,
                    error: None,
                },
            )
            .expect("the synthetic task block should retain its dependency");
            state.work_generation = state.work_generation.wrapping_add(1);
            exact
        };
        assert!(dependency.same_source(&source.dependency()));
        let outcome = source.subscriptions.subscribe_with(
            self.runtime,
            registration,
            || source.is_terminal(),
            before_insert,
        );
        let woke = if outcome == CompletionSubscriptionOutcome::AlreadyTerminal {
            self.wake_dependency_batch_guarded(
                &mutation,
                DependencyWakeBatch {
                    source: source.key(),
                    registrations: vec![registration],
                },
            )
        } else {
            false
        };
        drop(mutation);
        self.notify_dependency_wake(woke);
        registration
    }

    fn park_claimed_test_spark(
        &self,
        claimed: ClaimedSparkWork,
        source: &TestCompletionSource,
        before_insert: impl FnOnce(),
    ) -> Result<WakeRegistration, Box<ClaimedSparkWork>> {
        if source.runtime_id() != self.runtime {
            return Err(Box::new(claimed));
        }

        let mutation = self.admission.mutation_guard();
        let (registration, obsolete_dependency) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&claimed.id)
                .expect("claimed test spark work must remain registered");
            assert_eq!(record.id, claimed.id);
            assert_eq!(record.demand_session, claimed.session.id());
            assert!(matches!(record.state, WorkState::Running));
            assert!(record.control.close_reason.is_none());

            let current_dependency = source.dependency();
            let (dependency, obsolete_dependency) = if claimed
                .prior_dependency
                .as_ref()
                .is_some_and(|prior| prior.same_source(&current_dependency))
            {
                drop(current_dependency);
                (claimed.prior_dependency, None)
            } else {
                (Some(current_dependency), claimed.prior_dependency)
            };
            let record = state
                .work
                .get_mut(&claimed.id)
                .expect("claimed test spark work must remain registered");
            let spark = spark_work_mut(record);
            spark.demand = Some(claimed.demand);
            spark.dependency = dependency;
            record.subscription_epoch = record
                .subscription_epoch
                .checked_add(1)
                .expect("evaluation work subscription epochs exhausted");
            record.state = WorkState::Blocked;
            let registration = WakeRegistration {
                work: claimed.id,
                subscription_epoch: record.subscription_epoch,
            };
            state.work_generation = state.work_generation.wrapping_add(1);
            (registration, obsolete_dependency)
        };

        let outcome = source.subscriptions.subscribe_with(
            self.runtime,
            registration,
            || source.is_terminal(),
            before_insert,
        );
        let woke = if outcome == CompletionSubscriptionOutcome::AlreadyTerminal {
            self.wake_dependency_batch_guarded(
                &mutation,
                DependencyWakeBatch {
                    source: source.key(),
                    registrations: vec![registration],
                },
            )
        } else {
            false
        };
        drop(mutation);
        self.work_available.notify_all();
        self.notify_dependency_wake(woke);
        obsolete_dependency
            .into_iter()
            .for_each(WorkDependency::abandon);
        Ok(registration)
    }

    fn redeliver_test_registration(
        &self,
        source: WorkDependencyKey,
        registration: WakeRegistration,
    ) -> bool {
        let mutation = self.admission.mutation_guard();
        let changed = self.wake_dependency_batch_guarded(
            &mutation,
            DependencyWakeBatch {
                source,
                registrations: vec![registration],
            },
        );
        drop(mutation);
        self.notify_dependency_wake(changed);
        changed
    }
}

fn claimed_test_spark() -> (
    Arc<EvaluationWorkCoordinator>,
    Arc<super::super::EvaluationExecutor>,
    TestDemand,
    ClaimedSparkWork,
) {
    let (coordinator, executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    coordinator.executor_started(1);
    coordinator.submit_spark(session.demand.clone(), crate::core::keys::unit_value());
    let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
        panic!("test spark should be claimable")
    };
    (coordinator, executor, session, claimed)
}

fn publish_test_observation(coordinator: &EvaluationWorkCoordinator) -> RuntimeObservationEpoch {
    let mutation = coordinator.admission.mutation_guard();
    let epoch = coordinator.observations.advance();
    let changed = coordinator.publish_runtime_observation_guarded(&mutation, epoch);
    drop(mutation);
    coordinator.observations.notify_all();
    coordinator.notify_runtime_observation(changed);
    epoch
}

struct TestTaskMachine;

impl EvaluationTaskMachine for TestTaskMachine {
    fn poll(
        &mut self,
        _context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        panic!("coordinator lifecycle tests drive deferred polls explicitly")
    }
}

struct CountTaskPolls(Arc<AtomicUsize>);

impl EvaluationTaskMachine for CountTaskPolls {
    fn poll(
        &mut self,
        _context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        self.0.fetch_add(1, Ordering::AcqRel);
        EvaluationMachinePoll::Yielded
    }
}

fn activate_test_reflection(coordinator: &EvaluationWorkCoordinator, work: EvaluationWorkId) {
    coordinator
        .install_reflection_machine(work, Box::new(TestTaskMachine))
        .unwrap_or_else(|_| panic!("reserved test reflection must accept its machine"));
    assert!(coordinator.activate_reflection(work));
}

struct CheckDeferredDropLocks {
    coordinator: Weak<EvaluationWorkCoordinator>,
    dropped_without_runtime_locks: Arc<AtomicBool>,
}

impl EvaluationTaskMachine for CheckDeferredDropLocks {
    fn poll(
        &mut self,
        _context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        panic!("the coordinator test drives this machine's terminal poll")
    }
}

impl Drop for CheckDeferredDropLocks {
    fn drop(&mut self) {
        let unlocked = self.coordinator.upgrade().is_none_or(|coordinator| {
            let state_unlocked = coordinator.state.try_lock().is_ok();
            let admission_unlocked = coordinator.admission.try_settlement_guard().is_some();
            state_unlocked && admission_unlocked
        });
        self.dropped_without_runtime_locks
            .store(unlocked, Ordering::Release);
    }
}

struct CountDeferredDropLocks {
    coordinator: Weak<EvaluationWorkCoordinator>,
    drops: Arc<AtomicUsize>,
    all_drops_unlocked: Arc<AtomicBool>,
}

impl EvaluationTaskMachine for CountDeferredDropLocks {
    fn poll(
        &mut self,
        _context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        panic!("the coordinator test drives this machine explicitly")
    }
}

impl Drop for CountDeferredDropLocks {
    fn drop(&mut self) {
        let unlocked = self.coordinator.upgrade().is_none_or(|coordinator| {
            let state_unlocked = coordinator.state.try_lock().is_ok();
            let admission_unlocked = coordinator.admission.try_settlement_guard().is_some();
            state_unlocked && admission_unlocked
        });
        self.all_drops_unlocked
            .fetch_and(unlocked, Ordering::AcqRel);
        self.drops.fetch_add(1, Ordering::AcqRel);
    }
}

fn reserve_ready_test_reflection(
    coordinator: &EvaluationWorkCoordinator,
    session: &TestDemand,
) -> (EvaluationTaskId, EvaluationWorkId) {
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("reflection task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("reflection wait identity should allocate");
    let work = coordinator
        .reserve_reflection(&session.demand, task, wait)
        .expect("open test session should reserve reflection work");
    activate_test_reflection(coordinator, work);
    (task, work)
}

fn claim_ready_test_reflection(
    coordinator: &EvaluationWorkCoordinator,
    session: EvaluationSessionId,
) -> ClaimedReflectionWork {
    let ClaimedTaskWork::Reflection(claimed) = coordinator
        .claim_ready_task_for_session(session)
        .expect("queued reflection work should be claimable")
    else {
        panic!("queued reflection work should preserve its kind")
    };
    claimed
}

fn settle_test_reflection(coordinator: &Arc<EvaluationWorkCoordinator>, work: EvaluationWorkId) {
    coordinator.settle_terminal_work(
        work,
        EvaluationWaitTerminal::Cancelled,
        Arc::new(EvaluationFailure::message("test reflection settlement")),
    );
    drop(coordinator.retire_reflection(work));
}

fn settle_test_deferred(coordinator: &Arc<EvaluationWorkCoordinator>, work: EvaluationWorkId) {
    coordinator.settle_terminal_work(
        work,
        EvaluationWaitTerminal::Abandoned,
        Arc::new(EvaluationFailure::message("test deferred settlement")),
    );
    coordinator.retire_deferred(work);
}

fn finish_queued_test_spark(coordinator: &EvaluationWorkCoordinator) {
    let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
        panic!("woken test spark should be claimable")
    };
    coordinator.release_spark(claimed, SparkWorkPoll::Complete);
}

#[test]
fn completion_before_subscription_requeues_immediately() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let source = TestCompletionSource::new(&coordinator);
    source.complete();

    let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };

    assert_eq!(source.subscriber_count(), 0);
    assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
    finish_queued_test_spark(&coordinator);
}

#[test]
fn completion_during_subscription_cannot_lose_the_wake() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let source = TestCompletionSource::new(&coordinator);
    let started = Arc::new(Barrier::new(2));
    let completer = Arc::new(Mutex::new(None));

    let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, {
        let source = source.clone();
        let started = started.clone();
        let completer = completer.clone();
        move || {
            let completion_source = source.clone();
            let completion_started = started.clone();
            *completer
                .lock()
                .expect("completion thread slot was poisoned") = Some(thread::spawn(move || {
                completion_started.wait();
                completion_source.complete();
            }));
            started.wait();
            while !source.is_terminal() {
                thread::yield_now();
            }
        }
    }) else {
        panic!("same-runtime completion source should accept the subscription")
    };
    completer
        .lock()
        .expect("completion thread slot was poisoned")
        .take()
        .expect("the subscription hook should start a completer")
        .join()
        .expect("test completion thread should not panic");

    assert_eq!(source.subscriber_count(), 0);
    assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
    finish_queued_test_spark(&coordinator);
}

#[test]
fn task_completion_during_subscription_cannot_lose_the_wake() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let session_id = session.demand.id;
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session_id);
    let source = TestCompletionSource::new(&coordinator);
    let started = Arc::new(Barrier::new(2));
    let completer = Arc::new(Mutex::new(None));

    coordinator.park_claimed_test_reflection(claimed, &source, {
        let source = source.clone();
        let started = started.clone();
        let completer = completer.clone();
        move || {
            let completion_source = source.clone();
            let completion_started = started.clone();
            *completer
                .lock()
                .expect("completion thread slot was poisoned") = Some(thread::spawn(move || {
                completion_started.wait();
                completion_source.complete();
            }));
            started.wait();
            while !source.is_terminal() {
                thread::yield_now();
            }
        }
    });
    completer
        .lock()
        .expect("completion thread slot was poisoned")
        .take()
        .expect("the subscription hook should start a completer")
        .join()
        .expect("test completion thread should not panic");

    assert_eq!(source.subscriber_count(), 0);
    assert_eq!(coordinator.ready_task_count(), 1);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    assert!(
        coordinator
            .release_reflection(claimed, ReflectionWorkPoll::Terminal)
            .terminal
    );
    settle_test_reflection(&coordinator, work);
}

#[test]
fn completion_after_subscription_requeues_once() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let source = TestCompletionSource::new(&coordinator);
    let Ok(registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };
    assert_eq!(coordinator.spark_work_counts(), (0, 0, 1));
    assert_eq!(source.subscriber_count(), 1);

    source.complete();
    assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
    let generation = coordinator.work_generation();
    assert!(!coordinator.redeliver_test_registration(source.key(), registration));
    assert_eq!(coordinator.work_generation(), generation);
    finish_queued_test_spark(&coordinator);
}

#[test]
fn guarded_completion_defers_scheduler_notification_until_admission_is_released() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let source = TestCompletionSource::new(&coordinator);
    let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };
    assert_eq!(coordinator.spark_work_counts(), (0, 0, 1));

    let mutation = coordinator.admission.mutation_guard();
    let wake = source.complete_guarded(&coordinator, &mutation);
    assert!(source.is_terminal());
    assert_eq!(source.subscriber_count(), 0);
    assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));

    drop(mutation);
    wake.notify();
    finish_queued_test_spark(&coordinator);
}

#[test]
fn static_producer_obligations_are_taken_once() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("obligation task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("obligation wait identity should allocate");

    let mut reflection = SettlementObligations::reflection_task(wait.clone());
    let Some(ProducerSettlementObligation::ReflectionTask(publisher)) = reflection.take_producer()
    else {
        panic!("reflection inventory should contain its task wait")
    };
    assert_eq!(publisher.wait, wait);
    assert!(reflection.take_producer().is_none());

    let lazy = LazyValue::semantic_thunk(&session.demand.values, "static obligation", |_| {
        panic!("static obligation test never evaluates its synthetic lazy")
    });
    let producer = DeferredProducer::Lazy(lazy.root());
    let mut deferred = SettlementObligations::deferred_claim(wait.clone(), producer.clone());
    let Some(ProducerSettlementObligation::DeferredClaim {
        wait: obligation_wait,
        producer: obligation_producer,
    }) = deferred.take_producer()
    else {
        panic!("deferred inventory should contain its wait and claim")
    };
    assert_eq!(obligation_wait, wait);
    assert_eq!(obligation_producer.id(), producer.id());
    assert!(deferred.take_producer().is_none());
}

#[test]
fn terminal_settlement_publishes_once_before_reporting_retirement() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("settlement task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("settlement wait identity should allocate");
    let work = coordinator
        .reserve_reflection(&session.demand, task, wait.clone())
        .expect("open test session should reserve reflection work");
    activate_test_reflection(&coordinator, work);
    assert!(coordinator.terminalize_reflection(work));

    let terminal = coordinator.settle_terminal_work(
        work,
        EvaluationWaitTerminal::Cancelled,
        Arc::new(EvaluationFailure::message("settled test producer")),
    );
    assert_eq!(terminal, EvaluationWaitTerminal::Cancelled);
    assert_eq!(
        wait.terminal_poll(),
        Some(super::super::EvaluationWaitPoll::Cancelled)
    );
    assert!(matches!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::Terminalizing,
            ..
        }]
    ));

    drop(coordinator.retire_reflection(work));
    assert!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .is_empty()
    );
}

#[test]
fn session_close_does_not_steal_an_already_terminalizing_claims_settlement() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let session_id = session.demand.id;
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session_id);

    let release = coordinator.release_reflection(claimed, ReflectionWorkPoll::Terminal);
    assert!(release.terminal);
    drop(session);
    assert!(matches!(
        coordinator.reflection_snapshots(session_id).as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::Terminalizing,
            ..
        }]
    ));

    settle_test_reflection(&coordinator, work);
    assert_eq!(coordinator.registered_session_count(), 0);
}

#[test]
fn reflection_release_publishes_nonterminal_status_before_session_close() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let session_id = session.demand.id;
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let published = statuses.clone();
    assert!(coordinator.attach_reflection_lifecycle_publisher(
        work,
        TaskStatusPublisher::new(move |_mutation, status| {
            published.lock().unwrap().push(status);
            TaskStatusWake::new(|| {})
        }),
    ));
    let claimed = claim_ready_test_reflection(&coordinator, session_id);

    let release = coordinator.release_reflection(
        claimed,
        ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
            dependency: None,
            observed_epoch: Some(coordinator.current_observation_epoch()),
            error: None,
        }),
    );
    assert!(release.remains_blocked);
    assert!(!release.terminal);
    assert!(release.machine.is_none());
    assert_eq!(
        statuses.lock().unwrap().as_slice(),
        [EvaluationTaskStatus::Blocked]
    );

    drop(session);
    assert_eq!(
        statuses.lock().unwrap().as_slice(),
        [
            EvaluationTaskStatus::Blocked,
            EvaluationTaskStatus::Abandoned
        ]
    );
    assert!(coordinator.reflection_snapshots(session_id).is_empty());
}

#[test]
fn session_close_preserves_an_earlier_running_task_cancellation() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let session_id = session.demand.id;
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session_id);

    assert_eq!(
        coordinator.request_reflection_cancellation(work),
        ReflectionCancellation::Requested
    );
    drop(session);
    let release = coordinator.release_reflection(claimed, ReflectionWorkPoll::Terminal);
    assert!(release.terminal);
    assert!(release.cancel);
    assert!(!release.abandoned);

    settle_test_reflection(&coordinator, work);
    assert_eq!(coordinator.registered_session_count(), 0);
}

#[test]
fn stale_dependency_wake_does_not_requeue_work_blocked_elsewhere() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let source_a = TestCompletionSource::new(&coordinator);
    let source_b = TestCompletionSource::new(&coordinator);
    let Ok(registration_a) = coordinator.park_claimed_test_spark(claimed, &source_a, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };

    assert!(coordinator.redeliver_test_registration(source_a.key(), registration_a));
    let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
        panic!("the exact source delivery should requeue the test spark")
    };
    let Ok(registration_b) = coordinator.park_claimed_test_spark(claimed, &source_b, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };
    assert!(registration_b.subscription_epoch > registration_a.subscription_epoch);

    let generation = coordinator.work_generation();
    source_a.complete();
    assert_eq!(coordinator.work_generation(), generation);
    assert_eq!(coordinator.spark_work_counts(), (0, 0, 1));

    source_b.complete();
    assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
    finish_queued_test_spark(&coordinator);
}

#[test]
fn repeated_dependency_uses_a_new_epoch_and_queues_only_once() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let source = TestCompletionSource::new(&coordinator);
    let Ok(first) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };

    assert!(coordinator.redeliver_test_registration(source.key(), first));
    let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
        panic!("the exact source delivery should requeue the test spark")
    };
    let Ok(second) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };
    assert!(second.subscription_epoch > first.subscription_epoch);
    assert_eq!(source.subscriber_count(), 2);

    let generation = coordinator.work_generation();
    source.complete();
    assert_eq!(coordinator.work_generation(), generation.wrapping_add(1));
    assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
    assert!(!coordinator.redeliver_test_registration(source.key(), second));
    assert_eq!(coordinator.work_generation(), generation.wrapping_add(1));
    finish_queued_test_spark(&coordinator);
}

#[test]
fn retired_work_makes_late_completion_registrations_harmless() {
    let (coordinator, executor, session, claimed) = claimed_test_spark();
    let source = TestCompletionSource::new(&coordinator);
    let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };

    drop(session);
    assert_eq!(coordinator.retained_spark_count(), 0);
    source.complete();
    assert_eq!(coordinator.retained_spark_count(), 0);
    drop(executor);

    let (coordinator, executor, session, claimed) = claimed_test_spark();
    let source = TestCompletionSource::new(&coordinator);
    let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
        panic!("same-runtime completion source should accept the subscription")
    };

    drop(executor);
    assert_eq!(coordinator.retained_spark_count(), 0);
    source.complete();
    assert_eq!(coordinator.retained_spark_count(), 0);
    drop(session);
}

#[test]
fn completion_source_does_not_retain_its_runtime_coordinator() {
    let source = {
        let (coordinator, executor, session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };
        drop(session);
        drop(executor);
        drop(coordinator);
        source
    };

    assert!(!source.coordinator_is_live());
    source.complete();
    assert!(source.is_terminal());
    assert_eq!(source.subscriber_count(), 0);
}

#[test]
fn coordinator_publication_authority_does_not_retain_value_domain() {
    let values = CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    );
    let domain = Arc::downgrade(values.value_domain());
    let coordinator = EvaluationWorkCoordinator::new(
        &values,
        RuntimeMutationAdmission::new(),
        RuntimeObservationState::new(),
    );
    let observer = coordinator.value_observer();

    assert!(observer.is_live());
    drop(values);

    assert!(domain.upgrade().is_none());
    assert!(!observer.is_live());
}

#[test]
fn foreign_runtime_is_rejected_before_subscription_or_parking() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let other_values = CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    );
    let other_coordinator = EvaluationWorkCoordinator::new(
        &other_values,
        RuntimeMutationAdmission::new(),
        RuntimeObservationState::new(),
    );
    let source = TestCompletionSource::new(&other_coordinator);

    let Err(claimed) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
        panic!("foreign-runtime completion source must be rejected")
    };
    assert_eq!(source.subscriber_count(), 0);
    assert_eq!(coordinator.spark_work_counts(), (0, 1, 0));
    coordinator.release_spark(*claimed, SparkWorkPoll::Complete);
    assert_eq!(coordinator.retained_spark_count(), 0);
}

#[test]
fn foreign_promise_dependency_retires_work_without_subscribing() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let foreign_values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    );
    let promise = PromisedValue::new(&foreign_values, "foreign spark promise");

    coordinator.release_spark(
        claimed,
        SparkWorkPoll::Blocked(WorkDependency::Promise(promise.root())),
    );

    assert_eq!(promise.exact_subscription_count(), 0);
    assert_eq!(coordinator.retained_spark_count(), 0);
}

#[test]
fn foreign_wait_dependency_retires_work_without_subscribing() {
    let (coordinator, _executor, _session, claimed) = claimed_test_spark();
    let (foreign_coordinator, _foreign_executor) = super::super::test_execution_resources(0)
        .expect("foreign execution resources should build");
    let foreign_session = TestDemand::new(&foreign_coordinator);
    let producer = super::super::allocate_task_id(&foreign_session.demand.values)
        .expect("foreign producer identity should allocate");
    let wait = super::super::allocate_wait_token(&foreign_session.demand, producer)
        .expect("foreign wait identity should allocate");

    coordinator.release_spark(
        claimed,
        SparkWorkPoll::Blocked(WorkDependency::Wait(wait.clone())),
    );

    assert_eq!(wait.exact_subscription_count(), 0);
    assert_eq!(coordinator.retained_spark_count(), 0);
}

#[test]
fn coordinator_selects_exact_ready_work_without_a_session_queue() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("reflection task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("reflection wait identity should allocate");
    let work = coordinator
        .reserve_reflection(&session.demand, task, wait)
        .expect("open test session should reserve reflection work");
    activate_test_reflection(&coordinator, work);

    assert_eq!(coordinator.registered_session_count(), 1);
    assert_eq!(coordinator.ready_task_count(), 1);

    let CoordinatorSelection::Task(ClaimedTaskWork::Reflection(claimed)) = coordinator.select()
    else {
        panic!("the exact ready task should be selected")
    };
    assert_eq!(coordinator.ready_task_count(), 0);

    coordinator.requeue_unpolled_task(ClaimedTaskWork::Reflection(claimed));
    assert!(coordinator.terminalize_reflection(work));
    settle_test_reflection(&coordinator, work);
    drop(session);
    assert_eq!(coordinator.registered_session_count(), 0);
}

#[test]
fn serial_ready_selection_filters_exact_work_by_demand_session() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let left = TestDemand::new(&coordinator);
    let right = TestDemand::new(&coordinator);
    let (_, left_work) = reserve_ready_test_reflection(&coordinator, &left);
    let (right_task, right_work) = reserve_ready_test_reflection(&coordinator, &right);

    let right_claim = claim_ready_test_reflection(&coordinator, right.demand.id);
    assert_eq!(right_claim.task(), right_task);
    assert!(Arc::ptr_eq(&right_claim.demand.demand(), &right.demand));
    let release = coordinator.release_reflection(right_claim, ReflectionWorkPoll::Terminal);
    assert!(release.terminal);
    settle_test_reflection(&coordinator, right_work);

    let left_claim = claim_ready_test_reflection(&coordinator, left.demand.id);
    assert!(Arc::ptr_eq(&left_claim.demand.demand(), &left.demand));
    let release = coordinator.release_reflection(left_claim, ReflectionWorkPoll::Terminal);
    assert!(release.terminal);
    settle_test_reflection(&coordinator, left_work);
}

#[test]
fn claim_rejects_a_mismatched_registered_demand_before_machine_execution() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("reflection task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("reflection wait identity should allocate");
    let work = coordinator
        .reserve_reflection(&session.demand, task, wait)
        .expect("open test session should reserve reflection work");
    let polls = Arc::new(AtomicUsize::new(0));
    coordinator
        .install_reflection_machine(work, Box::new(CountTaskPolls(polls.clone())))
        .unwrap_or_else(|_| panic!("reserved test reflection must accept its machine"));
    assert!(coordinator.activate_reflection(work));

    let foreign_values = CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let mismatched = Arc::new(EvaluationDemandState {
        id: session.demand.id,
        values: foreign_values,
        default_reflection_profile: session.demand.default_reflection_profile.clone(),
        require_default_reflection_profile: false,
        closed: Arc::new(AtomicBool::new(false)),
        coordinator: Arc::downgrade(&coordinator),
        poll_contexts: AtomicUsize::new(0),
    });
    let original = coordinator
        .state
        .lock()
        .expect("evaluation work coordinator was poisoned")
        .demand_sessions
        .insert(session.demand.id, Arc::downgrade(&mismatched))
        .expect("the real demand session should already be registered");

    assert!(
        coordinator
            .claim_ready_task_for_session(session.demand.id)
            .is_none(),
        "a mismatched registered demand must be rejected"
    );
    assert_eq!(polls.load(Ordering::Acquire), 0);

    coordinator
        .state
        .lock()
        .expect("evaluation work coordinator was poisoned")
        .demand_sessions
        .insert(session.demand.id, original);
}

#[test]
fn coordinator_owns_the_reflection_lifecycle() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("reflection task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("reflection wait identity should allocate");
    let work = coordinator
        .reserve_reflection(&session.demand, task, wait)
        .expect("open test session should reserve reflection work");
    assert_eq!(
        coordinator.reflection_snapshots(session.demand.id),
        vec![ReflectionWorkSnapshot {
            task,
            state: ReflectionWorkState::Reserved,
        }]
    );

    activate_test_reflection(&coordinator, work);
    assert!(matches!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::Queued,
            ..
        }]
    ));

    let ClaimedTaskWork::Reflection(claimed) = coordinator
        .claim_ready_task_for_session(session.demand.id)
        .expect("queued reflection work should be claimable")
    else {
        panic!("queued reflection work should preserve its kind")
    };
    assert_eq!(claimed.id(), work);
    assert!(
        coordinator.claim_task(task).is_none(),
        "a running reflection work record must grant only one machine claim"
    );
    assert!(matches!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::Running,
            ..
        }]
    ));

    let observed = coordinator.observations.current();
    let block = EvaluationTaskBlock {
        dependency: None,
        observed_epoch: Some(observed),
        error: None,
    };
    let release =
        coordinator.release_reflection(claimed, ReflectionWorkPoll::Blocked(block.clone()));
    assert!(release.made_progress);
    assert!(release.remains_blocked);
    assert!(!release.terminal);
    assert_eq!(
        coordinator.reflection_snapshots(session.demand.id),
        vec![ReflectionWorkSnapshot {
            task,
            state: ReflectionWorkState::Blocked(block),
        }]
    );

    assert!(publish_test_observation(&coordinator) > observed);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    let release = coordinator.release_reflection(claimed, ReflectionWorkPoll::Terminal);
    assert!(release.terminal);
    assert!(matches!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::Terminalizing,
            ..
        }]
    ));
    settle_test_reflection(&coordinator, work);
    assert!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .is_empty()
    );
}

#[test]
fn coordinator_rejects_a_block_without_an_exact_or_broad_wake() {
    let mut state = WorkCoordinatorState::default();
    let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        publish_task_block_locked(
            &mut state,
            crate::runtime::allocate_evaluation_runtime_id(),
            EvaluationWorkId(NonZeroU64::new(1).expect("test work identity should be nonzero")),
            EvaluationTaskBlock {
                dependency: None,
                observed_epoch: None,
                error: None,
            },
        )
    }));
    assert!(rejected.is_err());
}

#[test]
fn observation_published_before_block_registration_requeues_on_recheck() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let observed = coordinator.observations.current();
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

    assert!(publish_test_observation(&coordinator) > observed);
    let release = coordinator.release_reflection(
        claimed,
        ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
            dependency: None,
            observed_epoch: Some(observed),
            error: None,
        }),
    );
    assert!(release.made_progress);
    assert!(!release.remains_blocked);
    assert_eq!(coordinator.ready_task_count(), 1);

    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    assert!(
        coordinator
            .release_reflection(claimed, ReflectionWorkPoll::Terminal)
            .terminal
    );
    settle_test_reflection(&coordinator, work);
}

#[test]
fn exact_wait_completion_requeues_only_its_cross_session_task() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let producer = TestDemand::new(&coordinator);
    let observer = TestDemand::new(&coordinator);
    let dependency_task = super::super::allocate_task_id(&producer.demand.values)
        .expect("dependency task identity should allocate");
    let dependency = super::super::allocate_wait_token(&producer.demand, dependency_task)
        .expect("dependency wait identity should allocate");
    let unrelated_task = super::super::allocate_task_id(&producer.demand.values)
        .expect("unrelated task identity should allocate");
    let unrelated = super::super::allocate_wait_token(&producer.demand, unrelated_task)
        .expect("unrelated wait identity should allocate");
    let (_, work) = reserve_ready_test_reflection(&coordinator, &observer);
    let claimed = claim_ready_test_reflection(&coordinator, observer.demand.id);

    let release = coordinator.release_reflection(
        claimed,
        ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
            dependency: Some(WorkDependency::Wait(dependency.clone())),
            observed_epoch: None,
            error: None,
        }),
    );
    assert!(release.remains_blocked);
    assert_eq!(dependency.exact_subscription_count(), 1);
    assert_eq!(coordinator.ready_task_count(), 0);

    unrelated.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
        &producer.demand.values,
        crate::core::keys::unit_value(),
    )));
    unrelated.notify_terminal();
    assert_eq!(coordinator.ready_task_count(), 0);
    dependency.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
        &producer.demand.values,
        crate::core::keys::unit_value(),
    )));
    dependency.notify_terminal();
    assert_eq!(dependency.exact_subscription_count(), 0);
    assert_eq!(coordinator.ready_task_count(), 1);

    let claimed = claim_ready_test_reflection(&coordinator, observer.demand.id);
    assert!(
        coordinator
            .release_reflection(claimed, ReflectionWorkPoll::Terminal)
            .terminal
    );
    settle_test_reflection(&coordinator, work);
}

#[test]
fn a_task_reblocked_on_another_wait_ignores_its_prior_terminal_source() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task_a = super::super::allocate_task_id(&session.demand.values)
        .expect("wait A task identity should allocate");
    let wait_a = super::super::allocate_wait_token(&session.demand, task_a)
        .expect("wait A identity should allocate");
    let task_b = super::super::allocate_task_id(&session.demand.values)
        .expect("wait B task identity should allocate");
    let wait_b = super::super::allocate_wait_token(&session.demand, task_b)
        .expect("wait B identity should allocate");
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

    assert!(
        coordinator
            .release_reflection(
                claimed,
                ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                    dependency: Some(WorkDependency::Wait(wait_a.clone())),
                    observed_epoch: None,
                    error: None,
                }),
            )
            .remains_blocked
    );
    assert_eq!(wait_a.exact_subscription_count(), 1);
    wait_a.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
        &session.demand.values,
        crate::core::keys::unit_value(),
    )));
    wait_a.notify_terminal();
    assert_eq!(wait_a.exact_subscription_count(), 0);
    assert_eq!(coordinator.ready_task_count(), 1);

    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    assert!(
        coordinator
            .release_reflection(
                claimed,
                ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                    dependency: Some(WorkDependency::Wait(wait_b.clone())),
                    observed_epoch: None,
                    error: None,
                }),
            )
            .remains_blocked
    );
    assert_eq!(wait_b.exact_subscription_count(), 1);
    assert_eq!(coordinator.ready_task_count(), 0);

    // Re-notifying the prior terminal source cannot revive work whose
    // subscription epoch now names wait B.
    wait_a.notify_terminal();
    assert_eq!(coordinator.ready_task_count(), 0);
    wait_b.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
        &session.demand.values,
        crate::core::keys::unit_value(),
    )));
    wait_b.notify_terminal();
    assert_eq!(coordinator.ready_task_count(), 1);

    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    assert!(
        coordinator
            .release_reflection(claimed, ReflectionWorkPoll::Terminal)
            .terminal
    );
    settle_test_reflection(&coordinator, work);
}

#[test]
fn exact_and_broad_task_wakes_share_one_block_epoch() {
    for exact_wins in [true, false] {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let observed = coordinator.observations.current();
        let dependency_task = super::super::allocate_task_id(&session.demand.values)
            .expect("dependency task identity should allocate");
        let dependency = super::super::allocate_wait_token(&session.demand, dependency_task)
            .expect("dependency wait identity should allocate");
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

        assert!(
            coordinator
                .release_reflection(
                    claimed,
                    ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                        dependency: Some(WorkDependency::Wait(dependency.clone())),
                        observed_epoch: Some(observed),
                        error: None,
                    }),
                )
                .remains_blocked
        );
        assert_eq!(dependency.exact_subscription_count(), 1);

        let complete = || {
            dependency.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
                &session.demand.values,
                crate::core::keys::unit_value(),
            )));
            dependency.notify_terminal();
        };
        if exact_wins {
            complete();
            publish_test_observation(&coordinator);
        } else {
            publish_test_observation(&coordinator);
            complete();
        }
        assert_eq!(dependency.exact_subscription_count(), 0);
        assert_eq!(coordinator.ready_task_count(), 1);

        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        assert!(
            coordinator
                .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                .terminal
        );
        settle_test_reflection(&coordinator, work);
    }
}

#[test]
fn retired_task_makes_a_late_exact_wait_wake_harmless() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let dependency_task = super::super::allocate_task_id(&session.demand.values)
        .expect("dependency task identity should allocate");
    let dependency = super::super::allocate_wait_token(&session.demand, dependency_task)
        .expect("dependency wait identity should allocate");
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

    assert!(
        coordinator
            .release_reflection(
                claimed,
                ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                    dependency: Some(WorkDependency::Wait(dependency.clone())),
                    observed_epoch: None,
                    error: None,
                }),
            )
            .remains_blocked
    );
    assert_eq!(
        coordinator.request_reflection_cancellation(work),
        ReflectionCancellation::Terminalize
    );
    settle_test_reflection(&coordinator, work);
    assert_eq!(dependency.exact_subscription_count(), 1);

    dependency.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
        &session.demand.values,
        crate::core::keys::unit_value(),
    )));
    dependency.notify_terminal();
    assert_eq!(dependency.exact_subscription_count(), 0);
    assert_eq!(coordinator.ready_task_count(), 0);
}

#[test]
fn observation_published_after_block_registration_requeues_exact_work() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let observed = coordinator.observations.current();
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    let dependency_task = super::super::allocate_task_id(&session.demand.values)
        .expect("dependency task identity should allocate");
    let dependency = super::super::allocate_wait_token(&session.demand, dependency_task)
        .expect("dependency wait identity should allocate");

    let release = coordinator.release_reflection(
        claimed,
        ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
            dependency: Some(WorkDependency::Wait(dependency.clone())),
            observed_epoch: Some(observed),
            error: None,
        }),
    );
    assert!(release.remains_blocked);
    assert_eq!(coordinator.ready_task_count(), 0);
    assert!(matches!(
        coordinator.reflection_snapshots(session.demand.id).as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(wait)),
                observed_epoch: Some(epoch),
                ..
            }),
            ..
        }] if wait == &dependency && *epoch == observed
    ));

    publish_test_observation(&coordinator);
    assert_eq!(coordinator.ready_task_count(), 1);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    assert!(
        coordinator
            .release_reflection(claimed, ReflectionWorkPoll::Terminal)
            .terminal
    );
    settle_test_reflection(&coordinator, work);
}

#[test]
fn permanent_exit_wait_retains_only_its_summary_and_obligations() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let (task, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    let message = RuntimeValueRoot::new(&session.demand.values, crate::core::keys::unit_value());

    let mut release = coordinator.release_reflection(
        claimed,
        ReflectionWorkPoll::Exit(EvaluationExitBlock {
            intent: ExitIntent::Error(message.clone()),
            observed_epoch: None,
        }),
    );
    assert!(release.exit_waiting);
    assert!(release.remains_blocked);
    assert!(!release.terminal);
    assert!(release.machine.is_some());
    {
        let state = coordinator
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let record = state
            .work
            .get(&work)
            .expect("exit-waiting work must remain registered");
        let reflection = reflection_work(record);
        assert!(matches!(record.state, WorkState::ExitWaiting));
        assert!(reflection.machine.is_none());
        assert!(reflection.block.is_none());
        assert_eq!(
            reflection.exit,
            Some(EvaluationExitBlock {
                intent: ExitIntent::Error(message),
                observed_epoch: None,
            })
        );
        assert!(record.obligations.producer.is_some());
        assert!(!state.observation_waiters.contains_key(&work));
        assert!(state.failures.is_empty());
    }
    drop(release.machine.take());

    coordinator.acknowledge_task_failure(session.demand.id, task);
    assert!(coordinator.failure_ledger_snapshot().is_empty());
    assert!(matches!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::ExitWaiting(EvaluationExitBlock {
                intent: ExitIntent::Error(_),
                observed_epoch: None,
            }),
            ..
        }]
    ));

    assert_eq!(
        coordinator.request_reflection_cancellation(work),
        ReflectionCancellation::Terminalize
    );
    settle_test_reflection(&coordinator, work);
}

#[test]
fn retryable_exit_wait_requeues_after_runtime_observation() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let observed = coordinator.current_observation_epoch();
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

    let release = coordinator.release_reflection(
        claimed,
        ReflectionWorkPoll::Exit(EvaluationExitBlock {
            intent: ExitIntent::Success,
            observed_epoch: Some(observed),
        }),
    );
    assert!(release.exit_waiting);
    assert!(release.remains_blocked);
    assert!(release.machine.is_none());
    assert!(matches!(
        coordinator.reflection_snapshots(session.demand.id).as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::ExitWaiting(EvaluationExitBlock {
                intent: ExitIntent::Success,
                observed_epoch: Some(epoch),
            }),
            ..
        }] if *epoch == observed
    ));

    publish_test_observation(&coordinator);
    assert_eq!(coordinator.ready_task_count(), 1);
    assert!(matches!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::Queued,
            ..
        }]
    ));

    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    assert!(
        coordinator
            .release_reflection(claimed, ReflectionWorkPoll::Terminal)
            .terminal
    );
    settle_test_reflection(&coordinator, work);
}

#[test]
fn observation_published_during_registration_is_caught_by_recheck() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let observed = coordinator.observations.current();
    let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    let mut epoch = coordinator.observations.lock_epoch_for_test();
    let releasing = {
        let coordinator = coordinator.clone();
        thread::spawn(move || {
            coordinator.release_reflection(
                claimed,
                ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                    dependency: None,
                    observed_epoch: Some(observed),
                    error: None,
                }),
            )
        })
    };

    while !coordinator
        .state
        .lock()
        .expect("evaluation work coordinator was poisoned")
        .observation_waiters
        .contains_key(&work)
    {
        thread::yield_now();
    }
    epoch.advance_for_test();
    drop(epoch);

    let release = releasing
        .join()
        .expect("observation release thread should finish");
    assert!(release.made_progress);
    assert!(!release.remains_blocked);
    assert_eq!(coordinator.ready_task_count(), 1);
    let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
    assert!(
        coordinator
            .release_reflection(claimed, ReflectionWorkPoll::Terminal)
            .terminal
    );
    settle_test_reflection(&coordinator, work);
}

#[test]
fn coordinator_cancels_reflection_reservations_without_polling() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("reflection task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("reflection wait identity should allocate");
    let work = coordinator
        .reserve_reflection(&session.demand, task, wait)
        .expect("open test session should reserve reflection work");

    assert_eq!(
        coordinator.request_reflection_cancellation(work),
        ReflectionCancellation::Terminalize
    );
    assert!(matches!(
        coordinator
            .reflection_snapshots(session.demand.id)
            .as_slice(),
        [ReflectionWorkSnapshot {
            state: ReflectionWorkState::Terminalizing,
            ..
        }]
    ));
    settle_test_reflection(&coordinator, work);
    assert_eq!(
        coordinator.request_reflection_cancellation(work),
        ReflectionCancellation::Late
    );
}

#[test]
fn coordinator_fairness_alternates_ready_tasks_and_sparks() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    coordinator.executor_started(1);
    coordinator.submit_spark(session.demand.clone(), crate::core::keys::unit_value());
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("reflection task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("reflection wait identity should allocate");
    let work = coordinator
        .reserve_reflection(&session.demand, task, wait)
        .expect("open test session should reserve reflection work");
    activate_test_reflection(&coordinator, work);

    let CoordinatorSelection::Task(claimed) = coordinator.select() else {
        panic!("task work should receive the first turn")
    };
    coordinator.requeue_unpolled_task(claimed);

    let CoordinatorSelection::Spark(spark) = coordinator.select() else {
        panic!("spark should receive the alternating turn")
    };
    coordinator.release_spark(spark, SparkWorkPoll::Complete);
    let CoordinatorSelection::Task(claimed) = coordinator.select() else {
        panic!("task work should receive the next alternating turn")
    };
    coordinator.requeue_unpolled_task(claimed);
    assert!(coordinator.terminalize_reflection(work));
    settle_test_reflection(&coordinator, work);
}

#[test]
fn queued_sparks_are_abandoned_when_their_demand_session_closes() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    coordinator.executor_started(1);
    coordinator.submit_spark(session.demand.clone(), crate::core::keys::unit_value());
    let demand = Arc::downgrade(&session.demand);
    let [work] = coordinator
        .state
        .lock()
        .expect("evaluation work coordinator was poisoned")
        .work
        .keys()
        .copied()
        .collect::<Vec<_>>()[..]
    else {
        panic!("one stable spark work ID should be registered")
    };
    assert_ne!(work.get(), 0);
    assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));

    drop(session);

    assert!(
        demand.upgrade().is_none(),
        "an unclaimed spark record must not retain its demand domain"
    );
    assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
    assert_eq!(coordinator.retained_spark_count(), 0);
}

#[test]
fn claimed_spark_keeps_its_demand_domain_alive_through_owner_close() {
    let (coordinator, _executor, session, claimed) = claimed_test_spark();
    let demand = Arc::downgrade(&session.demand);

    drop(session);

    let retained = demand
        .upgrade()
        .expect("the detached claim must temporarily retain its demand domain");
    assert!(Arc::ptr_eq(&retained, &claimed.demand_session()));
    drop(retained);

    coordinator.release_spark(claimed, SparkWorkPoll::Complete);
    assert!(
        demand.upgrade().is_none(),
        "releasing the final claim must release its temporary demand route"
    );
}

#[test]
fn deferred_insertion_is_immediately_dormant_and_promotable() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("deferred task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("deferred wait identity should allocate");
    let lazy = LazyValue::semantic_thunk(
        &session.demand.values,
        "coordinator deferred lifecycle",
        |_| panic!("coordinator lifecycle test never evaluates its synthetic lazy"),
    );
    let DeferredWorkReservation::New = coordinator
        .reserve_deferred(
            &session.demand,
            task,
            wait.clone(),
            DeferredProducer::Lazy(lazy.root()),
            Box::new(TestTaskMachine),
        )
        .expect("open test session should reserve deferred work")
    else {
        panic!("fresh deferred work should reserve a canonical record")
    };
    let work = coordinator
        .deferred_work_for_wait(&wait)
        .expect("new deferred work should retain its wait index");

    assert!(
        coordinator
            .claim_ready_task_for_session(session.demand.id)
            .is_none()
    );
    assert!(matches!(
        coordinator
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .work
            .get(&work)
            .map(|record| record.state),
        Some(WorkState::Dormant)
    ));
    assert!(coordinator.promote_deferred_wait(&wait));
    let ClaimedTaskWork::Deferred(claimed) = coordinator
        .claim_ready_task_for_session(session.demand.id)
        .expect("demand after atomic insertion should queue the producer")
    else {
        panic!("queued deferred work should preserve its kind")
    };
    let dependency_task = super::super::allocate_task_id(&session.demand.values)
        .expect("dependency task identity should allocate");
    let dependency = super::super::allocate_wait_token(&session.demand, dependency_task)
        .expect("dependency wait identity should allocate");
    dependency.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
        &session.demand.values,
        crate::core::keys::unit_value(),
    )));
    let release = coordinator.release_deferred(
        claimed,
        DeferredWorkPoll::Blocked(EvaluationTaskBlock {
            dependency: Some(WorkDependency::Wait(dependency)),
            observed_epoch: None,
            error: None,
        }),
    );
    assert!(!release.remains_blocked);
    assert!(!release.terminal);

    let ClaimedTaskWork::Deferred(claimed) = coordinator
        .claim_ready_task_for_session(session.demand.id)
        .expect("a terminal dependency should immediately requeue the producer")
    else {
        panic!("the requeued producer should preserve its deferred kind")
    };
    let release = coordinator.release_deferred(claimed, DeferredWorkPoll::Yielded);
    assert!(release.made_progress);
    assert!(!release.remains_blocked);
    let ClaimedTaskWork::Deferred(claimed) = coordinator
        .claim_ready_task_for_session(session.demand.id)
        .expect("a yielded queued demand should remain ready")
    else {
        panic!("the yielded producer should preserve its deferred kind")
    };
    let release = coordinator.release_deferred(claimed, DeferredWorkPoll::Terminal);
    assert!(release.terminal);
    settle_test_deferred(&coordinator, work);
}

#[test]
fn closing_owner_immediately_after_deferred_insertion_abandons_the_dormant_record() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let session_id = session.demand.id;
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("deferred task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("deferred wait identity should allocate");
    let lazy = LazyValue::semantic_thunk(
        &session.demand.values,
        "close after deferred insertion",
        |_| panic!("session-close test must not poll its deferred machine"),
    );
    assert!(matches!(
        coordinator
            .reserve_deferred(
                &session.demand,
                task,
                wait.clone(),
                DeferredProducer::Lazy(lazy.root()),
                Box::new(TestTaskMachine),
            )
            .expect("open test session should reserve deferred work"),
        DeferredWorkReservation::New
    ));

    drop(session);

    assert_eq!(coordinator.deferred_counts(session_id), (0, 0, 0));
    assert_eq!(
        wait.terminal_poll(),
        Some(super::super::EvaluationWaitPoll::Abandoned)
    );
    assert!(lazy.cached().is_none());
}

#[test]
fn racing_deferred_candidates_install_one_dormant_machine_and_drop_the_loser_unlocked() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let lazy =
        LazyValue::semantic_thunk(&session.demand.values, "racing deferred candidates", |_| {
            panic!("candidate race test drives its machine explicitly")
        });
    let barrier = Arc::new(Barrier::new(3));
    let drops = Arc::new(AtomicUsize::new(0));
    let all_drops_unlocked = Arc::new(AtomicBool::new(true));
    let mut candidates = Vec::new();
    for _ in 0..2 {
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("candidate task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("candidate wait identity should allocate");
        let coordinator = coordinator.clone();
        let demand = session.demand.clone();
        let lazy = lazy.clone();
        let barrier = barrier.clone();
        let drops = drops.clone();
        let all_drops_unlocked = all_drops_unlocked.clone();
        candidates.push(thread::spawn(move || {
            barrier.wait();
            let reservation = coordinator
                .reserve_deferred(
                    &demand,
                    task,
                    wait.clone(),
                    DeferredProducer::Lazy(lazy.root()),
                    Box::new(CountDeferredDropLocks {
                        coordinator: Arc::downgrade(&coordinator),
                        drops,
                        all_drops_unlocked,
                    }),
                )
                .expect("racing candidate should observe an open session");
            match reservation {
                DeferredWorkReservation::New => (true, wait),
                DeferredWorkReservation::Existing(canonical) => (false, canonical),
            }
        }));
    }
    barrier.wait();
    let outcomes = candidates
        .into_iter()
        .map(|candidate| candidate.join().expect("candidate should not panic"))
        .collect::<Vec<_>>();

    assert_eq!(outcomes.iter().filter(|(new, _)| *new).count(), 1);
    assert_eq!(outcomes[0].1, outcomes[1].1);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(all_drops_unlocked.load(Ordering::Acquire));

    let canonical = &outcomes[0].1;
    let work = coordinator
        .deferred_work_for_wait(canonical)
        .expect("the winning candidate should retain the canonical index");
    assert!(coordinator.promote_deferred_wait(canonical));
    let ClaimedTaskWork::Deferred(claimed) = coordinator
        .claim_ready_task_for_session(session.demand.id)
        .expect("the canonical machine should become claimable")
    else {
        panic!("the canonical work should remain deferred")
    };
    let release = coordinator.release_deferred(claimed, DeferredWorkPoll::Terminal);
    assert!(release.terminal);
    settle_test_deferred(&coordinator, work);
    drop(release);

    assert_eq!(drops.load(Ordering::Acquire), 2);
    assert!(all_drops_unlocked.load(Ordering::Acquire));
}

#[test]
fn deferred_claim_excludes_competitors_and_releases_its_machine_outside_runtime_locks() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("deferred task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("deferred wait identity should allocate");
    let lazy = LazyValue::semantic_thunk(
        &session.demand.values,
        "coordinator machine ownership",
        |_| panic!("coordinator ownership test never evaluates its synthetic lazy"),
    );
    let dropped_without_runtime_locks = Arc::new(AtomicBool::new(false));
    let DeferredWorkReservation::New = coordinator
        .reserve_deferred(
            &session.demand,
            task,
            wait.clone(),
            DeferredProducer::Lazy(lazy.root()),
            Box::new(CheckDeferredDropLocks {
                coordinator: Arc::downgrade(&coordinator),
                dropped_without_runtime_locks: dropped_without_runtime_locks.clone(),
            }),
        )
        .expect("open test session should reserve deferred work")
    else {
        panic!("fresh deferred work should reserve a canonical record")
    };
    let work = coordinator
        .deferred_work_for_wait(&wait)
        .expect("new deferred work should retain its wait index");
    assert!(coordinator.promote_deferred_wait(&wait));
    let ClaimedTaskWork::Deferred(claimed) = coordinator
        .claim_ready_task_for_session(session.demand.id)
        .expect("promoted deferred work should be claimable")
    else {
        panic!("claimed work should preserve its deferred kind")
    };
    assert!(
        coordinator
            .claim_ready_task_for_session(session.demand.id)
            .is_none(),
        "a detached deferred machine must exclude a competing claim"
    );

    let mut release = coordinator.release_deferred(claimed, DeferredWorkPoll::Terminal);
    assert!(release.terminal);
    coordinator.settle_terminal_work(
        work,
        EvaluationWaitTerminal::Abandoned,
        Arc::new(EvaluationFailure::message("test deferred settlement")),
    );
    let machine = release
        .machine
        .take()
        .expect("terminal deferred release must return its machine");
    drop(machine);
    assert!(dropped_without_runtime_locks.load(Ordering::Acquire));
    coordinator.retire_deferred(work);
}

#[test]
fn outer_block_promotes_one_canonical_deferred_producer() {
    let (coordinator, _executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let producer_session = TestDemand::new(&coordinator);
    let observer_session = TestDemand::new(&coordinator);
    let producer_task = super::super::allocate_task_id(&producer_session.demand.values)
        .expect("producer task identity should allocate");
    let producer_wait = super::super::allocate_wait_token(&producer_session.demand, producer_task)
        .expect("producer wait identity should allocate");
    let lazy = LazyValue::semantic_thunk(
        &producer_session.demand.values,
        "cross-session canonical producer",
        |_| panic!("coordinator promotion test does not evaluate its lazy"),
    );
    let DeferredWorkReservation::New = coordinator
        .reserve_deferred(
            &producer_session.demand,
            producer_task,
            producer_wait.clone(),
            DeferredProducer::Lazy(lazy.root()),
            Box::new(TestTaskMachine),
        )
        .expect("open producer session should reserve deferred work")
    else {
        panic!("first demand should reserve the canonical producer")
    };
    let producer_work = coordinator
        .deferred_work_for_wait(&producer_wait)
        .expect("new deferred work should retain its wait index");
    let duplicate_task = super::super::allocate_task_id(&observer_session.demand.values)
        .expect("duplicate task identity should allocate");
    let duplicate_wait =
        super::super::allocate_wait_token(&observer_session.demand, duplicate_task)
            .expect("duplicate wait identity should allocate");
    let DeferredWorkReservation::Existing(canonical_wait) = coordinator
        .reserve_deferred(
            &observer_session.demand,
            duplicate_task,
            duplicate_wait,
            DeferredProducer::Lazy(lazy.root()),
            Box::new(TestTaskMachine),
        )
        .expect("open observer session should reuse deferred work")
    else {
        panic!("a racing demand must reuse the canonical producer")
    };
    assert_eq!(canonical_wait, producer_wait);

    let observer_task = super::super::allocate_task_id(&observer_session.demand.values)
        .expect("observer task identity should allocate");
    let observer_wait = super::super::allocate_wait_token(&observer_session.demand, observer_task)
        .expect("observer wait identity should allocate");
    let observer_work = coordinator
        .reserve_reflection(&observer_session.demand, observer_task, observer_wait)
        .expect("open observer session should reserve reflection work");
    activate_test_reflection(&coordinator, observer_work);
    let ClaimedTaskWork::Reflection(claimed) = coordinator
        .claim_ready_task_for_session(observer_session.demand.id)
        .expect("observer reflection work should be ready")
    else {
        panic!("observer work should preserve its reflection kind")
    };
    let release = coordinator.release_reflection(
        claimed,
        ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
            dependency: Some(WorkDependency::Wait(producer_wait)),
            observed_epoch: None,
            error: None,
        }),
    );
    assert!(release.remains_blocked);
    let ClaimedTaskWork::Deferred(producer) = coordinator
        .claim_ready_task_for_session(producer_session.demand.id)
        .expect("publishing the outer dependency should promote its dormant producer")
    else {
        panic!("promoted producer should preserve its deferred kind")
    };
    let release = coordinator.release_deferred(producer, DeferredWorkPoll::Terminal);
    assert!(release.terminal);
    settle_test_deferred(&coordinator, producer_work);
    assert!(coordinator.terminalize_reflection(observer_work));
    settle_test_reflection(&coordinator, observer_work);
}

#[test]
fn dropping_executor_does_not_discard_coordinator_session_state() {
    let (coordinator, executor) =
        super::super::test_execution_resources(0).expect("test execution resources should build");
    let session = TestDemand::new(&coordinator);
    let task = super::super::allocate_task_id(&session.demand.values)
        .expect("reflection task identity should allocate");
    let wait = super::super::allocate_wait_token(&session.demand, task)
        .expect("reflection wait identity should allocate");
    let work = coordinator
        .reserve_reflection(&session.demand, task, wait)
        .expect("open test session should reserve reflection work");
    activate_test_reflection(&coordinator, work);
    drop(executor);

    let CoordinatorSelection::Task(claimed) = coordinator.select() else {
        panic!("dropping the executor must preserve ready task work")
    };
    coordinator.requeue_unpolled_task(claimed);
    assert_eq!(coordinator.registered_session_count(), 1);
    assert!(coordinator.terminalize_reflection(work));
    settle_test_reflection(&coordinator, work);
}
