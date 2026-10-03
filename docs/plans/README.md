# Implementation Plans

This directory retains substantial transition and implementation plans as
project history. Each plan states its own status and records completion as its
checkpoints land.

Plans explain how the implementation moved between designs; current semantic
and architectural documentation remains authoritative when an old plan and
the implemented system differ. Completed or abandoned plans may be deleted
when their historical value no longer justifies keeping them.

## Active Plans

No active implementation plan is promoted here at present. Preliminary and
deferred plans below remain candidates for later work.

## Recent Completed Plans

- [`GarbageCollectionRoadmap_2026-08-19.md`](GarbageCollectionRoadmap_2026-08-19.md)
  completed the non-moving stop-the-world collector and its runtime
  integration through C8 and Gate G4.
- [`GarbageCollectorImplementation_2026-08-19.md`](GarbageCollectorImplementation_2026-08-19.md)
  completed and verified the standalone collector subcrate through its C8
  tuning, safety, and dynamic-tool audit.
- [`GarbageCollectorIntegration_2026-08-19.md`](GarbageCollectorIntegration_2026-08-19.md)
  completed the I0-I13 migration of Glam values, roots, workers, reflection,
  interaction nets, and explicit runtime maintenance, passing Gate G4.
- [`GarbageCollectorPersistentEdgeTraits_2026-09-12.md`](GarbageCollectorPersistentEdgeTraits_2026-09-12.md)
  completed the explicit managed-edge ownership migration and closed its
  dynamic and release-cost verification under collector-integration I11D.2.
- [`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md)
  closed the regional ownership, fixture, schedule, exact-inventory, and full
  ordinary/aggressive workspace issues exposed by I11D.1.
- [`ResumableWhnfEvaluation_2026-09-12.md`](ResumableWhnfEvaluation_2026-09-12.md)
  replaced recursive and replaying WHNF demand with bounded regional work and
  durable owner checkpoints.
- [`InteractionNetCallableWhnfSpill_2026-09-16.md`](InteractionNetCallableWhnfSpill_2026-09-16.md)
  completed inline-first callable evaluation and managed-net linear
  `CallableCheckpoint(NetWhnfState)` topology only when a quantum suspends.
- [`PureInteractionNetConstruction_2026-09-20.md`](PureInteractionNetConstruction_2026-09-20.md)
  replaced generic isolated reflection search with pure builder state over the
  existing `ListEffect` choice/cut machinery and one hidden strict-netlist
  replay primitive.

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
- **Public diagnostic projection cleanup (deferred).** Review the transitional
  `Diagnostic::from_parts` policy which eagerly caches shallow conventional
  `message` and `line` views beside the authoritative structured emission.
  Decide which embedding conveniences remain explicit projections, and keep
  enrichment, viewer selection, viewport data, and final rendering in the
  configured logger or other last-moment policy boundary. This is not a
  prerequisite for collector integration; pull it forward only if the cached
  projections obstruct a GC ownership or access boundary.
