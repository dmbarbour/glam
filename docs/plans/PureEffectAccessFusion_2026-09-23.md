# Pure Effect Access Fusion Plan — 2026-09-23

Status: preliminary and deliberately deferred until after
[Value Representation Refinement](ValueRepresentationRefinement_2026-08-19.md).
Rebased 2026-10-05 on the current dispatch shortcut. An earlier draft assumed
a bounded multi-step fusion loop, which the resumable-WHNF work removed
(holistic pre-performance review, finding R4).
This plan was extracted from the resumable-WHNF plan (now retired; see
[`README.md`](README.md) Retired History) so effect-fusion performance work
does not block stack, budget, or GC work.

## Purpose

Investigate extending the regional-WHNF principle to consecutive standard
effect steps. A sequence of constructively callback-free effect reductions
may be able to share one matching value-access region, retain intermediate
values as regional raw values, and publish roots only when the quantum
yields, suspends, or crosses an orchestration boundary.

This is an optimization of the existing effect semantics. Today every effect
step is its own machine transition with durable roots on both sides, and the
generic request interpreter remains the reference semantics and the
production fallback. Deferring this plan changes neither Glam semantics nor
the ownership, scheduling, or budget contracts of resumable WHNF.

## Current Baseline

The reflection machine (`src/reflection/machine.rs`) decodes each request
through one shared decoder, then dispatches it in
`interpret_decoded_drive`. A dispatch shortcut skips one generic round trip
for four requests:

- `.seq` pushes its continuation and returns straight to decoding the
  operation, instead of building a `Drive` work item;
- `.r` with a pending Glam continuation demands the continuation and decodes
  the application directly, instead of `apply_roots` followed by `Drive`;
- `.get` and `.set` go straight to state-path work.

Every other request takes `interpret_request`. Each shortcut step is still
one machine transition: it passes through the generic step loop, clones the
active branch there (review finding R2), and keeps every intermediate value
as a durable root. Nothing retains managed access across steps, so nothing
here is "fused" in this plan's sense.

The test-only `forcing_unfused` switch disables the shortcut, and
`EFFECT_FUSION_BUDGET` survives only as a test chain length. Neither is an
independent oracle: both paths share the decoder, the continuation
representation, and the state-path work, and no test forces a budget yield
partway through a shortcut chain.

## Why This Follows Value Representation Refinement

The present cost model is dominated by today's large compatibility `Value`,
registered-root representation, and per-argument publication boundaries.
Value Representation Refinement intends to replace those with cheap compact
internal value words and representation-specific managed nodes. That changes
the cost of copying an intermediate value, retaining it regionally,
publishing a root, and traversing an effect checkpoint.

Implementing regional effect fusion first would therefore optimize a boundary
which is expected to change and would create another checkpoint
representation for the value transition to migrate. Perform the
representation transition first, then remeasure. The later investigation may
still reuse the regional-WHNF control shape, but it must choose its storage
and publication model against the refined values actually in production.

No other plan depends on this one; stack, budget, GC, and value-representation
work proceed without it.

## Prerequisites

From the holistic pre-performance review:

- **A performance harness with counters** (P1 item 5). PEAF1 measures with
  it; this plan does not build its own.
- **An effect step that moves its branch instead of cloning it** (R2). Until
  then the per-step clone dominates long chains and would mask what PEAF1
  measures. Land it before PEAF1.
- **One shared regional poll type** (E4). PEAF2's regional driver must reuse
  it rather than add another `{Ready, Boundary, Yielded, Failed}` enum. Land
  it before PEAF2.

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

## Phase PEAF0 — Post-Representation Rebase and Oracle

- Review the completed Value Representation Refinement and the current
  evaluator, reflection-machine, root, and budget boundaries.
- Inventory the generic and shortcut request paths as they then stand,
  including every request-argument and continuation-result publication.
- Reconcile this plan with any later changes to persistent-edge safety,
  moving/concurrent GC, or trace-immediate root frames.
- Update the eligible-operation list from constructive properties rather than
  preserving the provisional operation names above.
- Build a differential reference independent of the production decoder. A
  switch that only bypasses dispatch shares the decoder, continuation
  representation, and state-path work with the path under test, so it cannot
  catch a defect in any of them. Candidates: a small direct interpreter for
  the eligible requests over test values, or a seeded model of the request
  algebra compared at observable results, failures, budgets, and state.
- Add fixtures that force a budget yield and a suspension at every position
  inside an eligible chain, which the current tests never do.

Do not carry a pre-refinement checkpoint shape forward merely to preserve this
plan's terminology.

## Phase PEAF1 — Measurement and Decision Gate

Requires the harness and the branch-move fix above.

Measure managed-access entries, root publications, internal-value copies,
WHNF work units, request dispatches, and durable checkpoint publications for
long standard-effect chains under the dispatch shortcut. Include chains
which end normally, exhaust their budget, suspend at each possible boundary,
and cross into each ineligible operation family.

Compare the measured cost with ordinary evaluator and assembly workloads. If
the traffic is not material after value refinement, keep the shortcut,
record the evidence and measurement harness, and close this plan without a
new regional representation.

If implementation is justified, select the smallest regional work form which
can retain one uninterrupted effect prefix without holding access across any
orchestration boundary. Record the expected reduction and a rollback
criterion before changing production dispatch.

## Phase PEAF2 — Regional Driver Prototype

- Prototype a regional standard-effect work form analogous to
  `drive_regional`, using the caller's existing matching access, shared
  mutable budget, and the shared regional poll type.
- Keep intermediate refined values regional only for the duration of that
  access scope.
- Charge every request reduction and continuation application exactly once.
- On yield or suspension, publish one complete traceable durable checkpoint
  before access closes.
- On an ineligible request, publish the minimum complete handoff, close access,
  and delegate to the ordinary reflection/effect orchestration path.
- Keep the generic interpreter available as the fallback, and the PEAF0
  reference as the differential oracle.

The prototype must not open nested access to simplify its implementation and
must not grant effect machines new scheduler ownership.

## Phase PEAF3 — Production Cutover

- Force every eligible transition and boundary in isolation before enabling
  the regional path by default.
- Compare the regional path with the PEAF0 reference: results, failures,
  exact budgets, request order, handler state, and suspension identities.
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
- local `.get`/`.set` state and rollback behavior match the reference;
- error context and structured failure values match the reference;
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

1. measurements reject the optimization and keep the dispatch shortcut with
   a documented cost model; or
2. the regional path passes the differential matrix, materially reduces
   measured root/access traffic, and leaves every orchestration boundary
   outside managed access.

In either case, update current architecture only after the selected production
shape exists. Do not make future stack, budget, or GC work depend on the
optimization being implemented.
