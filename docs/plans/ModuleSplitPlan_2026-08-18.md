# Module Split Plan — 2026-08-18

Status: Phase 0 complete; Phase 1A is next.

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
policy changes merely because code moves between modules. Phase 1 separately
authorizes one deliberate API correction: executable command and configuration
policy moves out of the library, after generic host mechanisms replace its
private dependencies, and the accidental public `glam::cli` surface is removed.

## Package and Product Boundary

This remains one Cargo package containing one library crate and the primary
`glam` binary crate. There is no planned workspace or package split. Glam is
first a command-line tool and second an embeddable library; the library exists
to expose the assembler, evaluation runtime, values, sources, diagnostics, and
generic effect-host mechanisms without owning the executable's product policy.

The complete configuration model belongs to the binary. This includes
`GLAM_CONF` discovery, configuration assembly, `conf.env`, `conf.cli`,
`conf.log`, future `conf.ide`, shell completion integration, process I/O, and
exit policy. The library may expose generic operations used to implement those
features, but must not know their command names, configuration paths, command
models, rendering policy, or lifecycle ordering.

Rust library and binary targets are separate crates even when they share a
package. The binary therefore consumes the library's public API; Rust has no
package-private visibility spanning those targets. Phase 1 must close real
embedding gaps with narrow reusable APIs rather than expose `CoreValue`,
`EvalContext`, arbitrary opaque runtime values, or built-in front-end internals
merely so executable-owned code will compile.

The intended source shape is:

```text
src/
  lib.rs                         # embedding facade
  ...                            # library implementation modules
  bin/
    glam/
      main.rs                    # process entry and top-level dispatch
      batch.rs                   # assembly, output, settlement, exit policy
      rendering.rs               # default/fallback terminal rendering
      command_line/
        mod.rs                   # typed command dispatch facade
        adapters.rs              # shell adapters
        basic.rs                 # bootstrap completion and routing
        bootstrap.rs             # bootstrap option grammar
        completion.rs            # completion protocol and evidence
        model.rs                 # executable command models
        output.rs                # help and command output formatting
        configured/
          mod.rs                 # `conf.cli` expansion and completion
          effects.rs             # configured CLI effect vocabulary
          host.rs                # isolated invocation state and journal
          path.rs                # path readers and completion policy
          search.rs              # branch selection and ambiguity policy
          token.rs               # nested token parser facade
          token/                 # token effect implementation
      configuration/
        mod.rs                   # loading, assembly, and `conf.env`
        logger/
          mod.rs                 # `conf.log` integration
          effects.rs             # logger-visible host effects
          supervisor.rs          # ingress and logger lifecycle
        ide/                     # future `conf.ide`
```

Names below `command_line/configured` and `configuration/logger` may be refined
by their dependency maps, but their ownership direction is fixed. A separate
package is not an alternative under this plan. `ModuleInput`, source and
manifest facilities, `.g` inspection, and generic text-pattern/effect-search
mechanisms may remain in the library; their CLI routing belongs to the binary.

## Artifacts

1. [`ModuleSplitInventory_2026-08-18.md`](ModuleSplitInventory_2026-08-18.md)
   records every Rust file, its rough production/test size, role, cohesion, and
   any provisional seam.
2. This plan records the review method, ranked candidates, approved transition
   phases, and verification once the inventory is complete.
3. The cleanup review receives a resolution summary after approved splits land.

The dated inventory is evidence for this review. Enduring ownership
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
  explicitly reviewed change says otherwise. Phase 1 explicitly removes the
  executable-owned `glam::cli` facade after its generic dependencies are
  available through the embedding boundary.
- No new dependency cycle or generic dumping-ground module is introduced.
- The finished package has one library and one primary directory-form binary;
  executable configuration policy is private to `src/bin/glam/`.

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

## Decision Discipline

The inventory ranks candidates, but does not pre-authorize every provisional
child module. Each candidate begins with an exact dependency-map checkpoint.
That checkpoint may approve the proposed shape, revise it, combine or divide
later checkpoints, defer the candidate, or conclude that the file should
remain intact.

After every completed hotspot, review the remaining phases against the new
module graph. Do not mechanically follow an obsolete split proposal merely
because it appears later in this plan. In particular:

- child names remain provisional until their dependency map is approved;
- cross-cutting tests stay at the nearest common owner, while responsibility-
  local tests move with their implementation;
- a move which requires broadening visibility or introducing sibling cycles
  is evidence that the proposed seam is wrong; and
- `core.rs` remains outside this transition unless the expected GC/value-model
  work first supplies a stable ownership boundary.

No line-count ceiling is proposed. The outcome should be a smaller number of
strong ownership decisions, not the maximum possible number of files.

## Inventory Result — 2026-08-18

The inventory accounts for all 127 Rust files. It separates dedicated and
inline tests from production size, then classifies each file by role and
cohesion. The main findings are:

1. `main.rs` has strong private seams between batch/configuration orchestration,
   logger effects and supervision, and default rendering. The adjacent public
   `cli` module is also executable policy and belongs in the same binary tree,
   although its private evaluator and reflection dependencies require a
   boundary transition before it can move.
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
implementation phases prematurely. The next step is Phase 1A: turn the
executable boundary, including `main.rs` and `src/cli/`, into an exact
dependency map and staged move list before editing Rust.

## Follow-up Phases

### Phase 0 — Inventory and candidate selection

Status: complete (2026-08-18).

- Record the review contract and move policy.
- Account for every Rust source file exactly once.
- Separate approximate production and test size.
- Classify cohesion and produce hotspot dossiers.
- Rank candidates without treating size as an automatic split finding.

Result: all 127 files are represented in the linked inventory. The executable
boundary formed by `main.rs` and `src/cli/` is the first candidate;
`reflection.rs`, `api.rs`, and the combined evaluation/coordinator boundary
follow. Secondary candidates require fresh approval, while `core.rs` is
deferred.

### Phase 1 — Establish the binary-owned command and configuration boundary

#### Phase 1A — Exact dependency and move map

- Map every production item in `main.rs` to process entry/command dispatch,
  configuration loading, batch assembly/settlement, logger effect handling,
  logger supervision, or default rendering.
- Map every production item under `src/cli/` to bootstrap command parsing,
  command models, shell completion, configured `conf.cli` effects, isolated
  search, token parsing, path policy, or output formatting.
- Record call direction, owned state, callbacks, runtime/assembler handles,
  and the tests which establish each boundary.
- Record every use by `src/cli/` of private library state. The initial audit
  must cover at least private value/core conversion, direct `EvalContext`
  demand, construction of standard and specialization-owned effect requests,
  isolated-search host/context construction, restricted environment/log
  requests, text-pattern parsing, opaque path handles, and the runtime CLI
  invocation-ID allocator.
- Refine the approved `src/bin/glam/` child shape without changing its ownership
  direction. Do not retain configuration-specific code in the library merely
  to avoid adding an appropriate generic embedding operation.
- Identify cross-cutting orchestration which should remain in `main.rs` and
  reject any split requiring a generic shared-helper module.

Deliverable: an in-plan item-range/dependency table and an ordered list of
mechanical moves plus a list of exact generic library prerequisites. Review
that map before editing Rust.

#### Phase 1B — Establish the directory-form binary and default rendering

- Move the binary crate root from `src/main.rs` to `src/bin/glam/main.rs` without
  changing the target name or process behavior.
- Move terminal color, context/header formatting, and default diagnostic
  rendering to private `rendering` ownership in that binary tree.
- Preserve all output bytes, indentation, fallback policy, and public process
  behavior.
- Move renderer-local tests; keep end-to-end logger tests at their common owner.

This validates both Cargo target discovery and the private binary module shape
before moving lifecycle code. Confirm that `cargo check --lib` remains
independent of the binary and that binary unit tests remain discoverable.

#### Phase 1C — Extract binary-owned logger effects and supervision

- Move `MainEffects`, its request/snapshot/journal protocol, logger task host,
  `LogHost`, and logger supervisor ownership beneath
  `configuration/logger/` according to the approved map. Rename generic-looking
  items when their actual owner is the configured logger.
- Preserve diagnostic ingress lifetime, runtime FIFO semantics, settlement
  ordering, callback lock boundaries, and failure reporting.
- Partition unit tests by effect handler versus supervisor lifecycle while
  retaining cross-component shutdown tests.

Treat effect protocol and supervisor lifecycle as separate checkpoints if the
Phase 1A map shows that one can move without the other.

#### Phase 1D — Extract configuration and batch orchestration

- Move `GLAM_CONF` discovery, configuration loading and assembly, `conf.env`
  construction, and the interfaces used to invoke `conf.cli` and `conf.log`
  beneath `configuration/`.
- Move batch assembly, output, runtime pump/settlement/reporting, and final exit
  policy to `batch` only where they form directed child dependencies.
- Keep the executable entry point and top-level command dispatch readable in
  `main.rs`.
- Preserve the existing order of configuration, assembly output, runtime
  settlement, fallback rendering, logger completion, and exit-code selection.

#### Phase 1E — Complete the generic effect-host embedding boundary

Add only the reusable library facilities established by Phase 1A. Expected
categories, subject to that dependency map, are:

- handler-side runtime-local value construction and outer-WHNF demand without
  exposing `CoreValue` or `EvalContext`;
- safe immutable isolated-search host construction and a nested-search entry
  which preserves the current demand/dependency semantics;
- standard and specialization-owned effect construction needed for `.fail`
  and scoped close operations;
- an explicit reusable environment-and-diagnostic request subset;
- a constrained host-token/capability mechanism for invocation-local path
  handles, rather than public arbitrary opaque Rust values; and
- the shared capture-free text-pattern operation used by macros and the CLI.

Latch this boundary with an integration test that implements and runs a small
effect specialization as an external consumer using only public `glam` APIs.
The checkpoint is incomplete if the test imports implementation modules or if
configuration-specific vocabulary appears in the library API.

#### Phase 1F — Migrate the CLI implementation into the binary

- Move the bootstrap parser, command models, output formatting, completion
  protocol/adapters, configured `conf.cli` effects, isolated search, token
  parser, path policy, and their tests from `src/cli/` to
  `src/bin/glam/command_line/`.
- Use `command_line/configured/` for the effectful configuration parser; do not
  create a second library facade or duplicate command models during the final
  state.
- Partition the mechanical move according to the Phase 1A dependency graph.
  Pure leaf modules may move first, but the library must never depend on the
  binary and temporary forwarding must have an explicit removal checkpoint.
- Preserve configured parsing, ambiguity, completion, path-handle,
  diagnostic-evidence, and `--parse_cli` behavior exactly.
- Keep CLI unit tests beside the binary modules and retain process-level tests
  in `tests/`.

#### Phase 1G — Remove accidental library CLI policy

- Remove `pub mod cli` and every configuration/command-specific export from
  the library.
- Remove private compatibility entry points added only for the old in-library
  CLI, including the runtime CLI invocation-ID allocator if the completed host
  token design makes invocation identity binary-owned.
- Verify that the library no longer owns `GLAM_CONF`, `conf.env`, `conf.cli`,
  `conf.log`, or `conf.ide` policy. Generic value, source, diagnostic,
  effect-search, and text-pattern mechanisms remain library-owned.
- Treat removal of `glam::cli` as the deliberate pre-release API correction
  authorized by this plan, not as an accidental path-preservation failure.

#### Phase 1H — Tighten, document, and audit the executable boundary

- Narrow visibility introduced by the moves.
- Remove forwarding used only during extraction.
- Update `src/README.md`, assembly/diagnostic architecture, and CLI user docs to
  distinguish executable configuration ownership from the generic library
  mechanisms used to implement it.
- Run `cargo check --lib`, focused binary/CLI/logger tests, process integration
  tests, the external effect-host API test, and the full required gates.
- Confirm through Cargo metadata or target checks that the package still
  contains one library and the one primary `glam` binary; no package split is
  introduced.
- Review Phases 2–5 for drift before proceeding.

### Phase 2 — Split reflection protocol, lifecycle, and machine

#### Phase 2A — Exact dependency and move map

- Map the public specialization/host transaction protocol, scheduled effect
  lifecycle and launchers, task-machine/continuation state, transaction state,
  and request decoding.
- Account explicitly for existing `requests`, `search`, and `store` children.
- Decide whether transaction structures belong with the public effect protocol
  or the machine; keep continuation, cut, retry, and fixpoint state together.
- Record public re-exports and filtered tests before moving code.

#### Phase 2B — Extract specialization and host protocol

- Move `TaskSpecialization`, host/snapshot/journal/commit contracts, standard
  and reflection effect markers, and closely owned request result types.
- Preserve existing `reflection::*` paths through private modules/re-exports.

#### Phase 2C — Extract lifecycle and launchers

- Move effect lifecycle state, scheduled runs, result policy, run builders,
  and type-erased launchers as one ownership layer.
- Preserve weak/strong ownership, runtime settlement participation, terminal
  publication, and diagnostic routing.

#### Phase 2D — Extract the task machine

- Move branches, continuations, cut/reset/fix state, block/retry state,
  transactions, and request interpretation together.
- Do not alter deterministic alternative scheduling, transactional retry, or
  cycle/error recovery while moving the interpreter.

#### Phase 2E — Tests, visibility, and drift review

- Place protocol, lifecycle, and machine tests with their new owners; retain
  cross-layer effect tests at the reflection root.
- Tighten visibility, run reflection/store/public API coverage and full gates,
  then refresh the `api.rs` dossier.

### Phase 3 — Turn `api.rs` into an embedding facade

#### Phase 3A — Exact dependency and ownership map

- Map value/evaluator/net construction, diagnostic bus/ingress, runtime
  lifecycle/readiness reports, transactional input/output state, runtime
  resources, assembler construction, and module building.
- Decide whether runtime events are owned by the runtime child or are a sibling
  behind a narrow runtime interface; this unresolved direction must be settled
  before extraction.
- Inventory every public export and internal test helper so paths and
  visibility can be preserved intentionally.

#### Phase 3B — Extract value and evaluator facade

- Move `Value`, `EvaluatedValue`, `Values`, promise resolver, checked net
  builder, evaluator, and reflection inspector according to the approved map.
- Preserve the construction/demand/reflection boundary established by the
  Value Facade transition.

If the dependency map gives checked nets or promise resolution an independent
owner, split this checkpoint rather than forcing an oversized `value` child.

#### Phase 3C — Extract diagnostic publication infrastructure

- Move diagnostic envelopes/events/counts, bus/subscription, ingress, and
  routing ownership without changing callback or buffering semantics.
- Keep diagnostic value projection separate from default rendering policy.

#### Phase 3D — Extract runtime events and lifecycle reports

- Move input/output endpoint state, journals, delivery records/failures,
  readiness/deadlock/settlement reports, and the relevant runtime methods in
  the dependency order chosen by Phase 3A.
- Preserve mutation-admission order, callback-outside-lock rules, FIFO
  conflicts, durable delivery failures, and quiescence validation.

Split event transport from readiness/settlement if the map shows two directed
owners; do not create a monolithic replacement for `api.rs`.

#### Phase 3E — Extract assembler and module construction

- Move assembler builder/host/profile wiring, source/module construction, and
  compilation execution behind the public facade.
- Retain all current public paths and builder staging rules.

#### Phase 3F — Public boundary and test audit

- Partition the large inline tests according to their new owners while keeping
  integration-like facade tests at the root.
- Verify exported paths, narrow visibility, run `public_api`, CLI, macro,
  diagnostics, runtime-event, and full repository tests.
- Refresh the evaluation/coordinator dossier after the runtime types move.

### Phase 4 — Partition evaluation sessions and work coordination

#### Phase 4A — Combined dependency map and ordering decision

- Map both `evaluation.rs` and `evaluation/coordinator.rs` together: waits,
  promise producer obligations, task handles/status, client demand, reflection
  profiles, sessions/contexts, completion subscriptions, work records/queues,
  and readiness/settlement projection.
- Identify which types are passive protocol values and which own mutation or
  terminalization.
- Select an extraction order that leaves the coordinator as the sole owner of
  scheduling state and avoids bidirectional sibling dependencies.

#### Phase 4B — Extract passive completion and settlement protocol

- Move completion-subscription primitives and/or readiness/settlement snapshot
  types only as approved by Phase 4A.
- Preserve the atomic subscription protocol, mutation admission, and terminal
  publication ordering.

#### Phase 4C — Partition coordinator work kinds

- Separate work-kind records and claims only where the coordinator retains
  queue selection and state transition authority.
- Keep producer settlement obligations close to the work records they retire.

#### Phase 4D — Partition session-side waits, promises, tasks, and client demand

- Move session-side concepts in the dependency order established by Phase 4A.
- Preserve task status publication, promise abandonment semantics, cross-
  session same-runtime handles, and resumable client-demand claims.

#### Phase 4E — Extract session/context orchestration and close the boundary

- Leave `evaluation.rs` as a deliberate facade/ownership map.
- Relocate feature-local tests, retain concurrency and settlement tests at the
  nearest common owner, tighten visibility, and run deterministic ordering
  regressions plus the full gates.

### Phase 5 — Re-inventory and close the dated review

#### Phase 5A — Secondary-candidate decision

- Recompute file/production/test sizes and dependency directions after the four
  primary hotspots settle.
- Revisit only the secondary candidates recorded in the inventory.
- Add a small approved extraction phase for a secondary file only when it
  reduces ownership coupling or visibility; otherwise record why it remains
  cohesive or deferred.
- Keep `core.rs` deferred unless a separate GC/value-model transition has
  established its replacement boundary.

#### Phase 5B — Architecture and cleanup-review closure

- Update `src/README.md` and relevant architecture docs with only the module
  ownership that now exists.
- Record the result under the module-splitting item in
  `CodeCleanup_2026-08-15.md`.
- Audit public paths, visibility, module cycles, stale comments, and the absence
  of new catch-all modules.
- Run all focused suites and the repository-required format, clippy, and full
  test gates.
- Mark this dated plan complete while leaving future growth to a new dated
  module review.
