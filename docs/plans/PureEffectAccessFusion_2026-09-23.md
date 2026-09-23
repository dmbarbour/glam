# Pure Effect Access Fusion Plan — 2026-09-23

Status: preliminary and deliberately deferred until after
[Value Representation Refinement](ValueRepresentationRefinement_2026-08-19.md).
This plan was extracted from W6G.2 of the
[resumable-WHNF plan](ResumableWhnfEvaluation_2026-09-12.md) so effect-fusion
performance work does not block stack/budget closure or the remaining GC
integration work.

## Purpose

Investigate extending the regional-WHNF principle to consecutive standard
effect steps. A bounded sequence of constructively callback-free effect
reductions may be able to share one matching value-access region, retain
intermediate values as regional raw values, and publish roots only when the
quantum yields, suspends, or crosses an orchestration boundary.

This is an optimization of the existing effect semantics. The current bounded
W5 standard-effect fusion remains the reference implementation and the
production fallback. Deferring this plan changes neither Glam semantics nor
the ownership, scheduling, or budget contracts established by resumable WHNF.

## Why This Follows Value Representation Refinement

The present cost model is dominated by today's large compatibility `Value`,
registered-root representation, and per-argument publication boundaries.
Value Representation Refinement intends to replace those with cheap compact
internal value words and representation-specific managed nodes. That changes
the cost of copying an intermediate value, retaining it regionally, publishing
a root, and traversing an effect checkpoint.

Implementing regional effect fusion first would therefore optimize a boundary
which is expected to change and would create another checkpoint
representation for the value transition to migrate. Perform the
representation transition first, then remeasure. The later investigation may
still reuse the regional-WHNF control shape, but it must choose its storage and
publication model against the refined values actually in production.

This plan is not a prerequisite for the remainder of
`ResumableWhnfEvaluation_2026-09-12.md`, its W7/W8 gates, GC integration, or
Value Representation Refinement itself.

## Semantic Boundary

Begin with `.r`, `.seq`, and continuation application. Include `.get` and
`.set` only when they address the evaluator-local handler state represented by
the same pure effect machine; reflection volumes, transactional stores, host
resources, and other externally coordinated state are never admitted merely
because they use similar request names.

An operation is eligible only while it is constructively known to be:

- local and callback-free;
- free of coordinator, reflection, transaction, diagnostic-bus, or host
  publication;
- able to complete without waiting on an unavailable lazy, promise,
  reflection gate, or task;
- representable entirely by traceable regional state; and
- within the one shared deterministic work budget.

Lazy or promised results, reflection and task operations, heap or volume
operations, choice and control operations, and specialized requests initially
leave the regional driver through an explicit durable boundary. Broaden the
eligible set only after the same properties are proved for another family.
No optimization may keep managed access open across a callback, wait,
scheduler/coordinator operation, task launch, transaction, or host operation.

## Phase PEAF0 — Post-Representation Rebase

- Review the completed Value Representation Refinement and current evaluator,
  reflection-machine, root, and budget boundaries.
- Inventory the production generic and fused standard-effect paths, including
  every request-argument and continuation-result publication.
- Reconcile this plan with any later changes to persistent-edge safety,
  moving/concurrent GC, or trace-immediate root frames.
- Update the eligible-operation list from constructive properties rather than
  preserving the provisional operation names above.
- Build a differential unfused reference switch if the current one no longer
  exists.

Do not carry a pre-refinement checkpoint shape forward merely to preserve this
plan's terminology.

## Phase PEAF1 — Measurement and Decision Gate

Measure managed-access entries, root publications, internal-value copies,
WHNF work units, request dispatches, and durable checkpoint publications for
long standard-effect chains under the current bounded fusion. Include chains
which end normally, exhaust their budget, suspend at each possible boundary,
and cross into each ineligible operation family.

Compare the measured cost with ordinary evaluator and assembly workloads. If
the traffic is not material after value refinement, retain the bounded path,
record the evidence and measurement harness, and close this plan without a
new regional representation.

If implementation is justified, select the smallest regional work form which
can retain one uninterrupted effect prefix without holding access across any
orchestration boundary. Record the expected reduction and a rollback
criterion before changing production dispatch.

## Phase PEAF2 — Regional Driver Prototype

- Prototype a regional standard-effect work form analogous to
  `drive_regional`, using the caller's existing matching access and shared
  mutable budget.
- Keep intermediate refined values regional only for the duration of that
  access scope.
- Charge every request reduction and continuation application exactly once.
- On yield or suspension, publish one complete traceable durable checkpoint
  before access closes.
- On an ineligible request, publish the minimum complete handoff, close access,
  and delegate to the ordinary reflection/effect orchestration path.
- Keep the unfused interpreter available as a differential semantic oracle.

The prototype must not open nested access to simplify its implementation and
must not grant effect machines new scheduler ownership.

## Phase PEAF3 — Production Cutover

- Force every eligible transition and boundary in isolation before enabling
  the regional path by default.
- Compare the regional and unfused results, failures, exact budgets, request
  order, handler state, and suspension identities.
- Update source-backed access, root-publication, managed-edge, and machine
  inventories for the selected representation.
- Remove only compatibility code made unreachable by the cutover; retain the
  generic interpreter as the semantic path for ineligible operations and as a
  test oracle where its maintenance cost remains small.
- Remeasure the workloads from PEAF1 and revert the production cutover if the
  measured benefit does not justify the added state-machine surface.

## Verification Matrix

Differential and forced-order fixtures must prove that:

- an uninterrupted eligible chain does not register a root per effect step;
- every request and continuation application consumes deterministic shared
  budget;
- budget yield publishes one complete durable checkpoint and resumes without
  replay;
- forced suspension after each eligible step preserves completed state and the
  exact pending lazy or promise identity;
- every ineligible operation closes regional access before scheduler,
  reflection, transaction, diagnostic, or host coordination begins;
- local `.get`/`.set` state and rollback behavior match the unfused path;
- error context and structured failure values match the unfused path;
- cancellation, task/session closure, and unwind retain a complete traceable
  checkpoint or an already-published terminal result; and
- zero-, one-, and many-worker runs preserve the same semantic result without
  turning the regional driver into independently scheduled work.

Use barriers or explicit dependencies for disputed concurrent orderings;
repetition is stress evidence only. Run the routine repository gates, the
interaction-net profiling regressions, relevant aggressive-GC verification,
and Miri/model checks required by whichever post-refinement representations
the implementation touches.

## Completion Criteria

Close this plan in one of two valid states:

1. measurements reject the optimization and preserve the bounded W5 path with
   a documented cost model; or
2. the regional path passes the differential matrix, materially reduces
   measured root/access traffic, and leaves every orchestration boundary
   outside managed access.

In either case, update current architecture only after the selected production
shape exists. Do not make future stack, budget, or GC work depend on the
optimization being implemented.
