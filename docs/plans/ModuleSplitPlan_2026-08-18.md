# Module Split Plan — 2026-08-18

Status: inventory complete; transition phases pending review.

This is a dated review and transition plan. Module shape will continue to
change as the bootstrap grows, so a later review should create a new dated
plan rather than silently treating this inventory as permanent architecture.

The immediate purpose is to close the final module-structure item from
[`CodeCleanup_2026-08-15.md`](../reviews/CodeCleanup_2026-08-15.md): inspect
the settled implementation, identify files which still combine distinct
ownership roles, and plan low-risk splits along those boundaries.

## Intent

Review every Rust source file before selecting split targets. Large size is a
signal, not a finding. A file warrants division when the resulting child
modules would own coherent state, invariants, protocols, or semantic roles and
would reduce visibility or change coupling. A large implementation of one
coherent algorithm may remain intact.

The review must distinguish:

- production code from colocated tests;
- facade aggregation from implementation ownership;
- architectural responsibilities from merely repeated mechanisms;
- stable ownership seams from representations still scheduled to change; and
- navigation improvements from splits which would only add module ceremony.

No semantic behavior, public API, scheduling rule, lock ordering, or diagnostic
policy changes merely because code moves between modules.

## Artifacts

1. [`ModuleSplitInventory_2026-08-18.md`](ModuleSplitInventory_2026-08-18.md)
   records every Rust file, its rough production/test size, role, cohesion, and
   any provisional seam.
2. This plan records the review method, ranked candidates, approved transition
   phases, and verification once the inventory is complete.
3. The cleanup review receives a resolution summary after approved splits land.

The temporary inventory is evidence for this dated review. Enduring ownership
claims belong in `src/README.md` or `docs/architecture/` only after the new
module shape exists.

## Review Method

### Pass 0 — Mechanical census

For every `src/**/*.rs` file, record:

- total lines;
- approximate production and test lines where a terminal `mod tests` permits
  that distinction;
- module-tree position; and
- whether it is production, test-only, a facade/root, or an implementation
  leaf.

The counts prioritize inspection but do not establish desired file size.

### Pass 1 — File role inventory

Read the files by subsystem, including small neighboring files. For each file,
record:

- its primary responsibility;
- state and important types it owns;
- important entry points and collaborators;
- lock, lifecycle, or semantic invariants when relevant;
- a cohesion classification; and
- a provisional recommendation.

The classifications are:

- **cohesive** — one clear role, even if large;
- **facade** — intentionally gathers or re-exports several child roles;
- **mixed** — owns independently understandable responsibilities;
- **test-heavy** — raw size mostly reflects colocated verification; and
- **test-only** — verification organization rather than production ownership.

### Pass 2 — Hotspot dossiers

Deep-review every mixed file and every large production file. Map major item
clusters and ask:

1. Does each proposed child own a coherent concept rather than a grab bag?
2. Are dependencies mostly directed, or would the split create cyclic sibling
   imports and broad `pub(crate)` exposure?
3. Can implementation details become `pub(super)` or private?
4. Do tests naturally follow the extracted responsibility?
5. Can the first change be a mechanical move with public paths and behavior
   preserved?
6. Is the candidate likely to be invalidated by deferred architectural work?

### Pass 3 — Ranking and transition design

Rank candidates using, in order:

1. number of independent responsibilities;
2. strength and direction of internal seams;
3. visibility reduction;
4. improvement to change and test localization;
5. extraction risk; and
6. likelihood of near-term representation churn.

Do not introduce catch-all `common`, `shared`, or `util` modules. If a proposed
split needs one, revisit ownership first.

## Implementation Policy

Each approved hotspot gets its own low-risk checkpoints:

1. **Mechanical extraction.** Move one coherent cluster, retain existing
   public paths through the root module, and avoid behavior changes.
2. **Test relocation.** Move or partition tests according to the responsibility
   they verify while preserving useful filtered test names where practical.
3. **Boundary tightening.** Narrow `pub(crate)` to `pub(super)` or private only
   after the move is green and the new dependency direction is visible.
4. **Local cleanup.** Remove move-induced forwarding or stale comments without
   broadening the phase into an unrelated refactor.

Combining checkpoints is reasonable for a genuinely mechanical, small split.
Files with locks, unsafe code, lifecycle ownership, or large test fixtures
should remain partitioned.

## Success Criteria

- Every Rust file has been accounted for before candidates are approved.
- Root modules read as ownership maps or deliberate facades rather than
  miscellaneous containers.
- Child modules own state and operations together where practical.
- Splits reduce visibility or change coupling, not merely line count.
- Tests remain close to the responsibility they establish.
- Public paths and runtime semantics remain unchanged unless a separate,
  explicitly reviewed change says otherwise.
- No new dependency cycle or generic dumping-ground module is introduced.

## Verification

Before implementation, latch the current required checks. For each extraction,
run the narrow affected tests first, followed by:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Use `git diff --color-moved` or an equivalent move-aware review to distinguish
mechanical relocation from edits. Public-facade moves additionally run
`cargo test -q --test public_api`; parser, macro, CLI, reflection, evaluation,
and interaction-net moves run their corresponding focused targets or module
filters before the full suite.

## Deferred Decisions

The inventory will resolve:

- which files are actual split candidates;
- the desired child-module names and ownership directions;
- whether large inline test modules should move with production seams or remain
  as one child test module; and
- the implementation order.

No line-count ceiling is proposed. The outcome should be a smaller number of
strong ownership decisions, not the maximum possible number of files.

## Inventory Result — 2026-08-18

The inventory accounts for all 127 Rust files. It separates dedicated and
inline tests from production size, then classifies each file by role and
cohesion. The main findings are:

1. `main.rs` has strong private seams between batch/configuration orchestration,
   logger effects and supervision, and default rendering. It is the likely
   lowest-risk first split.
2. `reflection.rs` has durable seams between its public specialization/host
   protocol, effect lifecycle and launchers, and the continuation-based task
   machine.
3. `api.rs` mixes the largest number of ownership domains and offers the
   greatest payoff, but its public paths and runtime/event dependencies require
   careful staging.
4. `evaluation/coordinator.rs` and `evaluation.rs` should receive one combined
   dependency review before either is partitioned; their completion, task,
   promise, and session protocols deliberately cross the current file boundary.
5. Large evaluator, lexer, parser, persistent-list, and test files are often
   cohesive. They are not split solely for size.
6. `core.rs` is mixed but should remain deferred while garbage collection and
   the value representation are expected to change its natural ownership
   boundary.

Secondary candidates are recorded in the inventory rather than promoted to
implementation phases prematurely. The next planning step is to turn the
approved hotspot dossiers into exact child-module dependency maps and staged
move lists, beginning with whichever candidate is selected for implementation.
