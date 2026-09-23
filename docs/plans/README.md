# Implementation Plans

This directory retains substantial transition and implementation plans as
project history. Each plan states its own status and records completion as its
checkpoints land.

Plans explain how the implementation moved between designs; current semantic
and architectural documentation remains authoritative when an old plan and
the implemented system differ. Completed or abandoned plans may be deleted
when their historical value no longer justifies keeping them.

## Active Plans

- [`GarbageCollectionRoadmap_2026-08-19.md`](GarbageCollectionRoadmap_2026-08-19.md)
  coordinates the Glam-owned collector implementation and its integration into
  the runtime value domain.
- [`GarbageCollectorImplementation_2026-08-19.md`](GarbageCollectorImplementation_2026-08-19.md)
  builds and verifies the standalone collector subcrate.
- [`GarbageCollectorIntegration_2026-08-19.md`](GarbageCollectorIntegration_2026-08-19.md)
  migrates Glam values, roots, workers, reflection, and interaction nets.
- [`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md)
  closes the regional ownership, fixture, and schedule issues exposed by the
  integration plan's repository-wide aggressive collection mode.
- [`GarbageCollectorPersistentEdgeTraits_2026-09-12.md`](GarbageCollectorPersistentEdgeTraits_2026-09-12.md)
  is the nested GCI11R-002D transition from implicit `Gc<T>` traits to
  mutator-qualified persistent-edge duplication and identity.
- [`ResumableWhnfEvaluation_2026-09-12.md`](ResumableWhnfEvaluation_2026-09-12.md)
  replaces recursive and replaying WHNF demand with bounded regional work and
  durable owner checkpoints.
- [`InteractionNetCallableWhnfSpill_2026-09-16.md`](InteractionNetCallableWhnfSpill_2026-09-16.md)
  is the W6B.4b.2 subplan for inline-first callable evaluation and managed-net
  linear `CallableCheckpoint(NetWhnfState)` topology only when a quantum must
  suspend; its only reducing partner is the original `Bind`.
- [`PureInteractionNetConstruction_2026-09-20.md`](PureInteractionNetConstruction_2026-09-20.md)
  is the W6G.1f.3h subplan for replacing generic isolated reflection search
  with pure builder state over the existing `ListEffect` choice/cut machinery,
  followed by one hidden strict-netlist replay primitive.

## Recent Completed Plans

## Preliminary and Deferred Plans

- [`ConcurrentGarbageCollection_2026-08-28.md`](ConcurrentGarbageCollection_2026-08-28.md)
  records the post-integration transition from idle-only stop-the-world
  election to concurrent marking, delayed logical sweep, and epoch-safe run
  recycling across arbitrarily nested runtime heaps.
- [`ValueRepresentationRefinement_2026-08-19.md`](ValueRepresentationRefinement_2026-08-19.md)
  records the compact tagged-value and representation-splitting transition to
  pursue after the initial collector boundary works. It is deliberately not a
  prerequisite for the current GC plans.
- [`PureEffectAccessFusion_2026-09-23.md`](PureEffectAccessFusion_2026-09-23.md)
  retains the extracted W6G.2 measurement and regional-fusion investigation
  for pure standard-effect chains. It is deliberately sequenced after Value
  Representation Refinement and does not block resumable-WHNF closure.
- [`PublicResumableEvaluation_2026-09-23.md`](PublicResumableEvaluation_2026-09-23.md)
  consolidates the deferred library API for retaining, boundedly advancing,
  waiting on, and resuming one foreground evaluation without replaying its
  semantic work or exact producer route.
- [`GarbageCollectorScopedPointerSafety_2026-09-09.md`](GarbageCollectorScopedPointerSafety_2026-09-09.md)
  retains the deferred lifetime-branded `ScopedGc` experiment after the active
  persistent-edge trait migration establishes move-only stored edges.
