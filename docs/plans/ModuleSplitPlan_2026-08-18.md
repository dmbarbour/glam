# Module Split Plan — 2026-08-18

Status: Phase 2 complete; Phase 3A is next.

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

Status: complete (2026-08-18).

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

Resulting production ownership map:

| Current source | Current responsibility | Target owner |
| --- | --- | --- |
| `main.rs:32-363`, `2541-2562` | entry dispatch, completion/inspection/check-manifest commands, process argument and local-file helpers | `main.rs`, `command_line`, and thin command-specific adapters |
| `main.rs:364-473` | dormant runtime, assembler, configuration, diagnostic ingress, and canonical-environment preparation | `configuration` with the final handoff owned by `batch` |
| `main.rs:474-830` | assembly execution, output, manifest finalization, runtime settlement, and no-logger completion | `batch` |
| `main.rs:831-1019`, `1868-1931` | configuration module, `conf.env`, `conf.log` selection, configuration paths, and configuration error contexts | `configuration`, with logger launch under `configuration/logger` |
| `main.rs:1020-1204` | configured logger request vocabulary, snapshot, journal, and handler | `configuration/logger/effects` |
| `main.rs:1205-1726` | ingress lifecycle, fallback outbox, settled-report selection and conversion | `configuration/logger/supervisor` |
| `main.rs:1597-1867` | logger task host, runtime endpoints, transactional commit, and diagnostic consumption | `configuration/logger` split by the Phase 1C dependency checkpoint |
| `main.rs:1932-2540` | default diagnostic evaluation, structured context layout, and terminal color | `rendering` |
| `main.rs:2563-end` | renderer, logger, settlement, configuration, and process integration tests | responsibility-local binary tests plus cross-component tests at the binary root |
| `cli/{bootstrap,basic,adapters}.rs` | bootstrap grammar and shell-neutral/basic completion routing | `command_line` |
| `cli/{model,completion,output}.rs` | executable command model, completion evidence, help and output format | `command_line` |
| `cli/path.rs` | filesystem path acceptance and completion policy | `command_line/configured/path.rs` |
| `cli/{configured,effects,host,search}.rs` | `conf.cli` selection, effect vocabulary, branch journal, and ambiguity policy | `command_line/configured` |
| `cli/token.rs`, `cli/token/` | nested token-effect search | `command_line/configured/token.rs`, `token/` |
| `cli/tests.rs` | bootstrap, configured CLI, completion, token, and path contracts | binary command-line tests, partitioned only after the move is green |

The directed executable call graph is:

```text
main
  -> command_line
  -> configuration
       -> configuration::logger
  -> batch
       -> configuration::PreparedAssembly
       -> configuration::logger
       -> rendering (fallback only)

command_line::configured
  -> public glam value/effect/search mechanisms
  -> command_line::{model,completion}
```

The library never depends on any node in this graph. Cross-cutting
`PreparedAssembly` remains configuration-owned because it owns the loaded
configuration and unresolved environment promises; batch consumes it after
canonical arguments are selected.

The private dependency audit found these required replacements:

| Existing private dependency | Required generic boundary |
| --- | --- |
| `Values::core`, `Value::from_core`, direct core construction | existing public `Values` constructors plus missing semantic effect/token constructors |
| `RequestContext::eval_context` and `eval::eval_value` | request-context outer-WHNF demand returning public `EvaluatedValue`/structured `TaskHalt` |
| `IsolatedTaskHost::new` | safe public immutable isolated-search host construction |
| `IsolatedEffectSearch::new_in_context` | nested request-context search preserving current demand ownership; top-level configured search may own an explicit client-demand session |
| `g_syntax::fail_effect_value` | generic semantic standard-fail constructor |
| hand-built hidden case-close request | specialization-owned request-effect construction without exposing abstract-global keys |
| `environment_log_request_specs` | reusable restricted environment/diagnostic request profile |
| private `TextPattern` | narrow public capture-free text-pattern facility shared with macros |
| `OpaqueValue<PathHandle>` | constrained host-token domain whose values cannot be forged or downcast generically |
| runtime `next_cli_invocation` | binary-owned invocation identity, removable once path-token ownership is explicit |

Minor decisions recorded for review:

- `PreparedAssembly` is configuration-owned rather than a generic shared type.
- The default renderer remains separate from the configured logger lifecycle;
  it is process fallback policy, not part of `conf.log` semantics.
- The existing one-file CLI test suite moves intact first; responsibility-local
  partitioning happens only after the external-crate boundary is green.

Verification: the pre-move baseline completed with all 1,300 tests passing
(`cargo test -q`: 1,165 library/unit plus 135 binary/integration tests across
the remaining targets). `git diff --check` passed for the plan update.

#### Phase 1B — Establish the directory-form binary and default rendering

Status: complete (2026-08-18).

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

Result: Cargo now discovers `src/bin/glam/main.rs` as the package's sole
primary binary, while the library remains rooted at `src/lib.rs`.
`src/bin/glam/rendering.rs` owns the default formatter, context rendering,
terminal policy, and eight focused renderer tests. The binary root retains the
cross-component logger and settlement tests.

Verification:

- `cargo check --lib` and `cargo check --bin glam` passed after moving the root;
- `cargo metadata --no-deps --format-version 1` reported exactly the expected
  `lib` and `bin` target kinds;
- `cargo test -q --bin glam` passed all 22 binary unit tests, the same count as
  before extraction;
- `cargo clippy --bin glam --tests -- -D warnings`, `cargo fmt --check`, and
  `git diff --check` passed; and
- `src/main.rs` no longer exists and both binary source files are present.

#### Phase 1C — Extract binary-owned logger effects and supervision

Status: complete (2026-08-18).

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

Result: `configuration/logger/effects.rs` now owns the configured logger's
effect specialization, request/snapshot/journal protocol, and task host;
`configuration/logger/supervisor.rs` owns the long-lived diagnostic ingress,
logger lifecycle supervision, settlement-report conversion, and fallback
delivery; and `configuration/logger/mod.rs` owns configured logger startup and
the running logger handle. No logger policy moved into the library.

Tests are partitioned by responsibility: diagnostic-ingress counting, rearm,
and teardown tests live with the supervisor; output-bus isolation lives with
the effect host; and retry, settlement, and shutdown-order tests remain at the
binary root because they deliberately cross configuration, runtime, and
supervisor boundaries.

Verification: `cargo fmt --check`,
`cargo clippy --bin glam --tests -- -D warnings`, and
`cargo test -q --bin glam` passed; all 22 binary tests remain present.

#### Phase 1D — Extract configuration and batch orchestration

Status: complete (2026-08-18).

- Move `GLAM_CONF` discovery, configuration loading and assembly, `conf.env`
  construction, and the interfaces used to invoke `conf.cli` and `conf.log`
  beneath `configuration/`.
- Move batch assembly, output, runtime pump/settlement/reporting, and final exit
  policy to `batch` only where they form directed child dependencies.
- Keep the executable entry point and top-level command dispatch readable in
  `main.rs`.
- Preserve the existing order of configuration, assembly output, runtime
  settlement, fallback rendering, logger completion, and exit-code selection.

Result: `configuration/mod.rs` now owns `GLAM_CONF` discovery, the dormant
runtime and assembler construction, canonical process/reflection argument
promises, `conf.env` loading, configuration contexts, and the
`PreparedAssembly` handoff. `batch.rs` owns file finalization, worker
activation, assembly and stdout output, runtime settlement, fallback report
delivery, configured CLI execution, and process exit policy. `main.rs` is
reduced to top-level dispatch plus the still-to-be-moved command-specific
adapters.

Configuration-load failure crosses the boundary as a boxed internal
`PreparationFailure` carrying the partially constructed assembler, source
tracker, and log host. Batch can therefore publish and render the error and
finalize a requested manifest without giving configuration terminal output or
exit-code responsibilities. This is a private ownership handoff, not a new
semantic error category.

Verification: `cargo fmt --check`,
`cargo clippy --bin glam --tests -- -D warnings`, and
`cargo test -q --bin glam` passed; all 22 binary tests remain green, including
the cross-component settlement and logger-order tests.

#### Phase 1E — Complete the generic effect-host embedding boundary

Status: complete (2026-08-18).

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

Host-token checkpoint discovered during migration:

- The configured CLI currently returns a private `OpaqueValue<PathHandle>`
  from `.read.path` and later downcasts it in `.write.file` or
  `.write.manifest`. Moving that code across the crate boundary must not expose
  arbitrary opaque-value construction or downcasting, and encoding the handle
  as an ordinary dictionary would make a capability forgeable.
- The proposed generic boundary is an `EffectTokenDomain<T>` created for one
  runtime and one host-owned scope. It issues runtime-local opaque token
  values and resolves only tokens issued by that exact domain to `Arc<T>`.
  There is no generic inspection or downcast on `Value`.
- Token values hold only a domain-scoped ID and a weak domain reference. The
  domain owns the payload map; dropping the domain revokes every outstanding
  token, and dropping the last clone of one token removes its payload. This
  avoids a `Value -> payload -> Value/runtime` ownership cycle and gives token
  lifetime an explicit host owner.
- The configured CLI would put a fresh domain in each invocation snapshot.
  Path handles therefore cannot cross invocations by construction, allowing
  removal of `EvaluationRuntime::allocate_cli_invocation_id` and the
  CLI-specific runtime-global allocator.
- This is deliberately narrower than a `HostValue`: it is an effect-handler
  capability transport, not a persistence or IPC value, and it cannot be
  observed without possession of the issuing Rust domain.

The remaining Phase 1E boundaries are straightforward projections of existing
internals: public outer-WHNF demand and `Values` access on `RequestContext`, a
request-spec-owned constructor for hidden scoped-close effects, a standard
fail-effect constructor, immutable isolated-search host construction and
nested search, the restricted environment/diagnostic request profile, and the
shared capture-free text-pattern parser.

Result: the library now exposes runtime-local `EffectTokenDomain<T>`, public
immutable `IsolatedTaskHost`, standard fail-effect construction, request-spec
effect construction for specialization-owned hidden operations,
`RequestContext` value construction/outer-WHNF demand/nested search, the
restricted environment-and-diagnostic request profile, and the versioned
capture-free `TextPattern`. `Values::unit` and `EvaluatedValue::as_u64` fill
the corresponding ordinary semantic construction/extraction gaps without
exposing core representations.

`EffectTokenDomain<T>` uses weak token-to-domain references and domain-owned
payload records. It provides revocation and cleanup without allowing generic
opaque-value downcasts or creating token/payload ownership cycles. The CLI now
uses one path-token domain per invocation, and the obsolete CLI invocation ID
was removed from `RuntimeIds`.

Verification:

- `tests/effect_embedding.rs` implements and runs an external effect
  specialization using only public `glam` APIs;
- the focused token-domain test proves exact-domain resolution, rejection by
  another domain, rejection of ordinary values, and weak revocation ownership;
- a private-dependency scan of `src/cli/` finds no remaining `core`, `eval`,
  `evaluation`, `g_syntax`, raw `EvalContext`, opaque downcast, or CLI ID use;
- `cargo clippy --all-targets --all-features -- -D warnings` passed after the
  boundary changes; and
- the focused external embedding and token-domain tests passed.

#### Phase 1F — Migrate the CLI implementation into the binary

Status: complete (2026-08-18).

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

Result: bootstrap parsing, command models, output formatting, completion,
configured effects and search, token parsing, path policy, and their 46 unit
tests now live under `src/bin/glam/command_line/`. The effectful implementation
is grouped under `command_line/configured/`; pure bootstrap/basic completion
remains at the command-line root. Process-level contracts remain in
`tests/cli.rs`.

The temporary semantic mismatch found by the move was in the new embedding
facade, not CLI behavior: `Values::unit` initially constructed an empty list
instead of projecting the runtime's cached unit atom. The migrated success-path
tests reliably exposed it, and the constructor now returns the existing
semantic unit.

Verification: `cargo clippy --all-targets --all-features -- -D warnings`
passed, `cargo test -q --bin glam` passed all 68 binary tests (the prior 22 plus
46 moved CLI tests), and `cargo test -q --test cli` passed all 49 process-level
CLI tests.

#### Phase 1G — Remove accidental library CLI policy

Status: complete (2026-08-18).

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

Result: `src/cli.rs`, `src/cli/`, and `pub mod cli` are removed. All binary
callers use the private `command_line` module directly. The CLI-specific
runtime invocation allocator and its `RuntimeIds` field are gone; path token
identity is invocation-domain-local instead. No forwarding facade remains.

Verification: source scans find no `glam::cli`, `crate::cli`,
`allocate_cli_invocation`, or `next_cli_invocation` references. A policy scan
outside `src/bin/` finds no executable ownership of `GLAM_CONF`, `conf.cli`,
`conf.log`, or `conf.ide`; the lone `conf.env` occurrence is evaluator test
data. Cargo metadata still reports one `lib` target and one `glam` `bin`
target in the same package.

#### Phase 1H — Tighten, document, and audit the executable boundary

Status: complete (2026-08-18).

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

Result: every command-line model and operation is now at most binary-crate
visible; the direct `batch` and `configuration` handoffs are parent-visible,
while nested logger items remain crate-visible only where the sibling batch or
cross-component tests consume them. No extraction-only forwarding module or
compatibility entry point remains. The binary compiles cleanly through the
private `command_line`, `configuration`, `batch`, and `rendering` ownership
graph.

`src/README.md` now maps the directory-form binary and its private policy
modules. The assembly and diagnostic architecture documents explicitly
separate executable-owned configuration, completion, configured logging,
rendering, and exit behavior from the generic library mechanisms used to
implement them. `docs/CLI.md` records that this is a library/binary crate
boundary inside one package, not a package split. The assembly agent context
and cleanup-review links now point at the moved tests and configured hosts.

Verification:

- `cargo check --lib` passed independently of the binary;
- the 46 command-line unit tests and four logger-local tests passed;
- all 49 process CLI integration tests passed;
- `tests/effect_embedding.rs` passed as an external public-API consumer;
- `cargo fmt --check` and
  `cargo clippy --all-targets --all-features -- -D warnings` passed;
- `cargo test -q` passed all 1,302 tests (1,120 library, 68 binary, and 114
  integration tests); and
- Cargo metadata reports one `lib` target at `src/lib.rs` and one `glam` `bin`
  target at `src/bin/glam/main.rs` in the same package.

Post-Phase-1 drift review:

- **Phase 2 remains correctly ordered.** Phase 1E deliberately added public
  `EffectRequestSpec` construction, `RequestContext` demand/search helpers,
  `IsolatedTaskHost`, and `EffectTokenDomain`. Phase 2A must include these in
  the specialization/host-protocol map; Phase 2B should move them with that
  protocol rather than strand embedding facilities in the machine or lifecycle
  layer. This enlarges the inventory, not the extraction risk or dependency
  direction.
- **Phase 3's intended seams are stronger.** `Values`, `EvaluatedValue`, and
  evaluator roles now follow the completed Value Facade boundary, while the
  default renderer and all configured diagnostic policy have left `api.rs`.
  Phase 3A should treat generic diagnostic transport as library-owned and must
  not recreate an executable logger or rendering child. No phase reorder is
  needed.
- **Phase 4 has no Phase-1-dependent semantic drift.** The external effect-host
  test now provides an additional contract for any reflection/evaluation move:
  isolated demand and nested search must remain usable without private
  `EvalContext` access. The combined dependency map is still the appropriate
  first checkpoint for the two large files.
- **Phase 5 must inventory the new binary tree.** Its census should distinguish
  the 46 command-line tests and logger/rendering tests now colocated with the
  executable from library production size. The package-boundary documentation
  is already current, but Phase 5 still owns the final post-split audit and
  cleanup-review closure.

### Phase 2 — Split reflection protocol, lifecycle, and machine

#### Phase 2A — Exact dependency and move map

Status: complete (2026-08-18).

- Map the public specialization/host transaction protocol, scheduled effect
  lifecycle and launchers, task-machine/continuation state, transaction state,
  and request decoding.
- Account explicitly for existing `requests`, `search`, and `store` children.
- Decide whether transaction structures belong with the public effect protocol
  or the machine; keep continuation, cut, retry, and fixpoint state together.
- Record public re-exports and filtered tests before moving code.

Resulting ownership map:

| Current range/child | Responsibility | Target owner |
| --- | --- | --- |
| `reflection.rs:63-494` | specialization request specifications/results, reasoning identity, host snapshots/commits, host/environment contracts, task outcome and structured halt | `reflection/protocol.rs` |
| `reflection.rs:495-1114` | host-observable lifecycle, scheduled/synchronous effect runs, coordinator-root activation, result policy, and type-erased launchers | `reflection/lifecycle.rs` |
| `reflection.rs:1115-4278` | effect vocabulary, task interpreter, branches/cuts/retry/fix/reset/continuations, transaction state, request context and decoding | divided between `reflection/machine.rs` and the protocol-owned transaction boundary below |
| `reflection/requests.rs` | reusable reflection request family and reflection-specific journal/host services | remains a protocol-adjacent child |
| `reflection/search.rs` | isolated all-results policy, immutable isolated host, and pollable search wrapper | remains a machine-adjacent child |
| `reflection/store.rs` | persistent reflection volumes, queries, journals, and conflict analysis | remains the independent store child |
| `reflection.rs:4279-end` | shared cross-layer effect harness and 104 protocol/machine/lifecycle integration tests | move intact to `reflection/machine/tests.rs`; feature-local child tests remain in `requests`, `search`, and `store` |

The dependency direction is:

```text
reflection facade
  -> protocol -> requests, search API, store, evaluation value/wait types
  -> machine  -> protocol, requests, search policy, store, evaluator
  -> search   -> protocol + machine
  -> lifecycle -> protocol + machine + evaluation coordinator facade
```

`RequestActivity`, `Transaction<S>`, `RequestContext`, and
`TransactionContext` move to `protocol`, despite currently appearing beside
the machine. They contain only host snapshots, reflection-store journals, and
specialization journals; none contains branch, continuation, cut, reset, or
fixpoint state. Keeping them with the public host contract removes what would
otherwise be a protocol-to-machine dependency. The small sealed request-value
constructor used by `EffectRequestSpec::effect` moves with them and is shared
privately with request decoding.

All branch, control, retry, fixpoint, continuation, reset-frame, task-block,
and terminal state moves together to `machine`. Request decoding and the
standard effect API also stay there because their tags and outcomes directly
drive that state machine. Lifecycle depends on the machine to construct and
erase tasks; the machine does not depend on lifecycle publication.

Public paths remain rooted at `reflection::*` through explicit facade
re-exports. The Phase 1E additions are accounted for as follows:

- `EffectRequestSpec`, `RequestContext`, and their public construction/demand
  helpers move with `protocol`;
- `IsolatedTaskHost` remains in `search`, re-exported by the facade;
- `EffectTokenDomain` remains in `api.rs` for the forthcoming Phase 3 ownership
  map—it transports public runtime values and is not part of reflection task
  interpretation; and
- `TextPattern` remains its independent library facility.

The latched reflection suite contains 134 tests: 104 cross-layer root tests,
five request tests, one search test, and 24 store tests. Moving the root suite
intact first avoids manufacturing a test-support facade merely to classify
integration tests by filename. Pure request/search/store tests are already
owned locally; lifecycle-only tests may move later only if they cease to rely
on the shared full-machine host fixture.

#### Phase 2B — Extract specialization and host protocol

Status: complete (2026-08-18).

- Move `TaskSpecialization`, host/snapshot/journal/commit contracts, standard
  and reflection effect markers, and closely owned request result types.
- Preserve existing `reflection::*` paths through private modules/re-exports.

Result: `reflection/protocol.rs` now owns specialization request
specifications/results, reasoning identity, host snapshots and commits,
task environment/host contracts, task outcomes and structured halts, plus the
host/store transaction and public request contexts. The latter move eliminates
a protocol-to-machine dependency: a transaction contains no interpreter
control state. Existing `reflection::*` paths are explicit facade re-exports.

Verification: `cargo check --lib`, formatting, and all 134 filtered reflection
tests passed after the extraction.

#### Phase 2C — Extract lifecycle and launchers

Status: complete (2026-08-18).

- Move effect lifecycle state, scheduled runs, result policy, run builders,
  and type-erased launchers as one ownership layer.
- Preserve weak/strong ownership, runtime settlement participation, terminal
  publication, and diagnostic routing.

Result: `reflection/lifecycle.rs` owns lifecycle status publication,
coordinator terminal policies, scheduled and synchronous composed runs, result
policy, and both direct and coordinator-capable type-erased task launchers.
The layer constructs machine tasks but the machine has no lifecycle
dependency. Public paths and the crate-private coordinator launcher remain
facade re-exports.

Verification: formatting and all 134 filtered reflection tests passed,
including lifecycle terminal publication, observer drop, abandonment, child
failure, and diagnostic-consumer activation cases.

#### Phase 2D — Extract the task machine

Status: complete (2026-08-18).

- Move branches, continuations, cut/reset/fix state, block/retry state,
  transactions, and request interpretation together.
- Do not alter deterministic alternative scheduling, transactional retry, or
  cycle/error recovery while moving the interpreter.

Result: `reflection/machine.rs` owns the effect vocabulary, persistent task
interpreter, branch/cut/retry/fix/reset/continuation state, blocking and
terminal records, and request decoding. Transaction snapshots and journals
remain protocol-owned because they contain no interpreter control state. The
machine depends on the protocol and focused request/search/store children;
the protocol and machine do not depend on lifecycle publication.

#### Phase 2E — Tests, visibility, and drift review

Status: complete (2026-08-18).

- Place protocol, lifecycle, and machine tests with their new owners; keep the
  cross-layer harness beside the machine whose private state it exercises.
- Tighten visibility, run reflection/store/public API coverage and full gates,
  then refresh the `api.rs` dossier.

Result: the 104 shared effect tests live in `reflection/machine/tests.rs`.
They inspect private interpreter state and exercise protocol and lifecycle
through the real machine, so colocating the harness with its machine owner is
clearer than adding a broad root-level test-support facade. The five request,
one search, and 24 store tests remain beside their focused owners. Production
modules now use explicit imports and sibling-only visibility for internal
construction and polling surfaces. `reflection.rs` is a small facade which
retains the established public and crate-private paths.

Verification: all 134 focused reflection tests, public embedding/API and macro
protocol tests, logger-filtered binary tests, formatting, full-feature Clippy,
and the complete `cargo test -q` suite pass after the split.

Post-Phase-2 drift review: the Phase 3 dossier remains current.
`EffectTokenDomain` still belongs with public runtime-value construction in
`api.rs`, while diagnostic transport and runtime event ownership remain the
unresolved seams Phase 3A must map. The reflection split introduced no new
dependency from the value facade to interpreter internals.

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
