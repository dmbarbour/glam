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
- [`InteractionNetFunctionCalls_2026-08-31.md`](InteractionNetFunctionCalls_2026-08-31.md)
  restores ordinary value-level function calls through explicitly constructed
  interaction nets without exposing function staging or changing raw-net
  loading.

## Recent Completed Plans

(deleted)

## Preliminary and Deferred Plans

- [`ConcurrentGarbageCollection_2026-08-28.md`](ConcurrentGarbageCollection_2026-08-28.md)
  records the post-integration transition from idle-only stop-the-world
  election to concurrent marking, delayed logical sweep, and epoch-safe run
  recycling across arbitrarily nested runtime heaps.
- [`ValueRepresentationRefinement_2026-08-19.md`](ValueRepresentationRefinement_2026-08-19.md)
  records the compact tagged-value and representation-splitting transition to
  pursue after the initial collector boundary works. It is deliberately not a
  prerequisite for the current GC plans.
