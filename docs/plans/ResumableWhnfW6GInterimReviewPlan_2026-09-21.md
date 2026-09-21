# W6G Interim Implementation Review Plan — 2026-09-21

Baseline: `e7cf6e0a`. Review the implementation present at this revision;
record later edits separately. The output is
[`ResumableWhnfW6GInterim_2026-09-21.md`](../reviews/ResumableWhnfW6GInterim_2026-09-21.md).
Status: complete on 2026-09-21.

This is an **as-built, mid-phase** review of completed W6G.1 checkpoints and
W6G.3, not closure of W6G.1 or a substitute for W6G.5. The W6G.1 baseline and
design reviews established intent before most family migrations; revisit
their conclusions against current code and tests. Classify unfinished 3i,
2b, 4, e.2/e.3, g, and h as transitional unless an implemented invariant is
already violated. W6G.2 and W6G.4 remain outside this review.

## Passes

1. **Boundary inventory.** Map completed checkpoints to current types and
   call paths: foreground registry and exact claim; managed lazy
   source/checkpoint/result; producer-family dispatch; reflection completion;
   aggregate rooted WHNF demand. Identify temporary coordinator machines,
   routes, adapters, and old compatibility representations still in use.
2. **Ownership and tracing audit.** Check one authoritative semantic owner per
   lazy, registered roots versus traced edges, task-owned reflection promises,
   host and spark post-access handoffs, and aggregate WHNF seed/promotion/poll
   lifetime. Distinguish deliberate temporary ownership from an untraced
   backedge, leaked root, or replayable one-shot action.
3. **Execution and verification audit.** Trace bounded poll, yield, exact
   dependency, route loss, terminal publication, and collection. Map each
   disputed ordering to a deterministic fixture or mark a verification gap;
   uncontrolled repeated test passes are not race evidence. Check source
   inventories for what they structurally prove and what they merely count.
4. **Drift and findings.** Compare actual behavior with the *completed*
   checkpoint contracts, not only the ultimate target. Give each finding code
   evidence, impact, confidence, and an owner: repair now, an existing W6G.1
   checkpoint, W6G.5, or a later plan. Explicitly identify intentional drift
   and whether it changes the remainder of the plan.
5. **Close the review.** Link it from the parent W6G plan. Keep W6G.5's final
   integrated verification, performance attribution, and W7/W8 drift review
   intact; carry forward only concrete unresolved items.

This is primarily a code-and-test review. Focused tests may be run to confirm
an identified mismatch, but a broad suite is not evidence for a disputed
ordering without a latch, barrier, or equivalent forced schedule.
